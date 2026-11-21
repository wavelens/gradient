/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, cache_upstream, ids::*, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows::{cache, cache_with_id, user};
use gradient_sources::{
    CacheSigner, format_cache_public_key, generate_signing_key, verify_narinfo_signature,
};
use gradient_test_support::state::TestState;
use gradient_test_support::web::spawn_loopback_http_server_with_connection_counter;
use gradient_types::{NixPathInfo, parse_narinfo_body};
use tempfile::TempDir;

const HELLO_STORE_PATH_HASH: &str = "brj5bb4pny8pnngq3qdymkllwql6z29j";
const SECOND_STORE_PATH_HASH: &str = "7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a";
const NARINFO_KEYS_WRITTEN_IN_ORDER: [&str; 11] = [
    "StorePath",
    "URL",
    "Compression",
    "FileHash",
    "FileSize",
    "NarHash",
    "NarSize",
    "References",
    "Deriver",
    "Sig",
    "CA",
];

fn store_path(hash: &str) -> String {
    format!("/nix/store/{hash}-hello-2.12")
}

struct TestUpstreamCacheSigningKey {
    signer: CacheSigner,
    public_key: String,
}

fn new_test_upstream_cache_signing_key(crypt_file: &str) -> TestUpstreamCacheSigningKey {
    let (private_key, public_key) = generate_signing_key(crypt_file).expect("upstream key");
    let upstream_cache = cache::Model {
        private_key,
        public_key,
        ..cache_with_id(CacheId::now_v7(), "upstream", cache().created_by)
    };
    let serve_url = "https://upstream.example";
    TestUpstreamCacheSigningKey {
        signer: CacheSigner::from_cache(crypt_file, &upstream_cache, serve_url)
            .expect("upstream signer"),
        public_key: format_cache_public_key(crypt_file, upstream_cache, serve_url.into())
            .expect("upstream public key"),
    }
}

fn unsigned_hello_store_path_narinfo() -> NixPathInfo {
    NixPathInfo {
        store_path: store_path(HELLO_STORE_PATH_HASH),
        url: "nar/1abc.nar.xz".into(),
        compression: "xz".into(),
        file_hash: Some("sha256:1njv6dxi9xm2blq6gq7y7kyij3q3nm4kpbgw4nfr2ldwqxvzm2c3".into()),
        file_size: Some(50088),
        nar_hash: "sha256:0yzhigwjl6bws649vcs2asa4lbs8hg93hyix187gc7s7a74w5h80".into(),
        nar_size: 226552,
        references: vec![format!("{SECOND_STORE_PATH_HASH}-glibc-2.38")],
        signature_lines: Vec::new(),
        deriver: Some(format!("{HELLO_STORE_PATH_HASH}-hello-2.12.drv")),
        ca: Some("fixed:r:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf".into()),
    }
}

fn add_test_upstream_cache_key_signature_to_narinfo(
    upstream: &TestUpstreamCacheSigningKey,
    mut path_info: NixPathInfo,
) -> NixPathInfo {
    let signature = upstream.signer.sign_parsed_narinfo(&path_info);
    path_info.signature_lines.push(signature);
    path_info
}

struct PullThroughCache {
    db: TestDatabase,
    _secrets: TempDir,
    server: TestServer,
    public_key: String,
    upstream: CacheUpstreamId,
}

async fn pull_through_cache_with_upstream_serving(
    upstream_body: impl FnOnce(&TestUpstreamCacheSigningKey) -> String,
) -> PullThroughCache {
    let secrets = TempDir::new().expect("a secrets directory");
    let crypt_file = secrets.path().join("crypt");
    std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").expect("the secret is written");
    let crypt_file = crypt_file.to_string_lossy().into_owned();
    let upstream = new_test_upstream_cache_signing_key(&crypt_file);
    let body: &'static str = Box::leak(upstream_body(&upstream).into_boxed_str());
    let (upstream_url, _) = spawn_loopback_http_server_with_connection_counter(body);

    let db = TestDatabase::create().await;
    let conn = db.connection();
    gradient_db::rows::insert::<user::Entity, _>(&conn, user())
        .await
        .expect("insert user");
    let (private_key, public_key) = generate_signing_key(&crypt_file).expect("Cache key");
    gradient_db::rows::insert::<cache::Entity, _>(
        &conn,
        cache::Model {
            public: true,
            pull_through: true,
            private_key,
            public_key: public_key.clone(),
            ..cache()
        },
    )
    .await
    .expect("insert cache");
    let upstream_row = gradient_db::rows::insert::<cache_upstream::Entity, _>(
        &conn,
        cache_upstream::Model {
            id: CacheUpstreamId::now_v7(),
            cache: cache().id,
            display_name: "upstream".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            kind: CacheUpstreamKind::Http,
            url: Some(upstream_url),
            public_key: Some(upstream.public_key.clone()),
            active: true,
            ..Default::default()
        },
    )
    .await
    .expect("insert upstream cache");

    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn.clone())
        .cache_db(conn)
        .cli(move |cli| {
            cli.secrets.crypt_file = crypt_file;
            cli.outbound.allowed_non_public_network_targets = vec!["127.0.0.0/8".into()];
        })
        .server();
    PullThroughCache {
        db,
        _secrets: secrets,
        server,
        public_key,
        upstream: upstream_row.id,
    }
}

async fn narinfo_text(setup: &PullThroughCache, hash: &str) -> axum_test::TestResponse {
    setup
        .server
        .get(&format!("/cache/{}/{hash}.narinfo", cache().name))
        .await
}

fn find_gradient_public_key_in_narinfo_sig_lines(
    setup: &PullThroughCache,
    response: &NixPathInfo,
) -> String {
    let key_name = response
        .signature_lines
        .iter()
        .filter_map(|line| line.split_once(':').map(|(name, _)| name))
        .find(|name| name.ends_with(&format!("-{}", cache().name)))
        .expect("Gradient signature");
    format!("{key_name}:{}", setup.public_key)
}

#[tokio::test]
async fn upstream_narinfo_with_two_store_paths_without_gradient_signatures_is_rejected() {
    for (first, second) in [
        (HELLO_STORE_PATH_HASH, SECOND_STORE_PATH_HASH),
        (SECOND_STORE_PATH_HASH, HELLO_STORE_PATH_HASH),
    ] {
        let setup = pull_through_cache_with_upstream_serving(|upstream| {
            let signed_for_path = add_test_upstream_cache_key_signature_to_narinfo(
                upstream,
                unsigned_hello_store_path_narinfo(),
            )
            .to_nix_string()
            .unwrap();
            let mut lines: Vec<String> = signed_for_path.lines().map(str::to_owned).collect();
            lines.retain(|line| !line.starts_with("StorePath: "));
            lines.insert(0, format!("StorePath: {}", store_path(second)));
            lines.insert(0, format!("StorePath: {}", store_path(first)));
            lines.join("\n") + "\n"
        })
        .await;

        for hash in [HELLO_STORE_PATH_HASH, SECOND_STORE_PATH_HASH] {
            let response = narinfo_text(&setup, hash).await;
            assert_eq!(
                response.status_code(),
                404,
                "StorePath {first} then {second}, asked for {hash}: {}",
                response.text()
            );
            assert!(!response.text().contains("Sig:"), "{}", response.text());
        }
        setup.db.finish().await;
    }
}

#[tokio::test]
async fn resigned_upstream_narinfo_gradient_signature_verifies_against_served_narinfo_fields() {
    let setup = pull_through_cache_with_upstream_serving(|upstream| {
        add_test_upstream_cache_key_signature_to_narinfo(
            upstream,
            unsigned_hello_store_path_narinfo(),
        )
        .to_nix_string()
        .unwrap()
    })
    .await;

    let response = narinfo_text(&setup, HELLO_STORE_PATH_HASH).await;
    response.assert_status_ok();
    let served = parse_narinfo_body(&response.text()).expect("The response is a valid narinfo.");

    assert!(verify_narinfo_signature(
        &find_gradient_public_key_in_narinfo_sig_lines(&setup, &served),
        &served
    ));
    assert_eq!(served.store_path, store_path(HELLO_STORE_PATH_HASH));
    assert_eq!(
        served.url,
        format!("nar/upstream/{}/nar/1abc.nar.xz", setup.upstream)
    );
    let tampered = NixPathInfo {
        references: Vec::new(),
        ..served.clone()
    };
    assert!(!verify_narinfo_signature(
        &find_gradient_public_key_in_narinfo_sig_lines(&setup, &served),
        &tampered
    ));
    setup.db.finish().await;
}

#[tokio::test]
async fn resigned_upstream_narinfo_has_only_gradient_keys_in_gradient_order() {
    let setup = pull_through_cache_with_upstream_serving(|upstream| {
        let signed = add_test_upstream_cache_key_signature_to_narinfo(
            upstream,
            unsigned_hello_store_path_narinfo(),
        )
        .to_nix_string()
        .unwrap();
        format!("{signed}System: x86_64-linux\n")
    })
    .await;

    let response = narinfo_text(&setup, HELLO_STORE_PATH_HASH).await;
    response.assert_status_ok();
    let text = response.text();

    let served = parse_narinfo_body(&text).expect("The response is a valid narinfo.");
    assert_eq!(served.to_nix_string().unwrap(), text);
    for line in text.lines() {
        let key = line.split_once(": ").map_or(line, |(key, _)| key);
        assert!(
            NARINFO_KEYS_WRITTEN_IN_ORDER.contains(&key),
            "unknown key {key}: {text}"
        );
    }
    assert_eq!(served.signature_lines.len(), 2, "{text}");
    setup.db.finish().await;
}

#[tokio::test]
async fn upstream_narinfo_without_file_hash_file_size_and_compression_none_is_re_signed_and_served()
{
    let setup = pull_through_cache_with_upstream_serving(|upstream| {
        let uncompressed = NixPathInfo {
            compression: "none".into(),
            file_hash: None,
            file_size: None,
            url: "nar/1abc.nar".into(),
            ..unsigned_hello_store_path_narinfo()
        };
        add_test_upstream_cache_key_signature_to_narinfo(upstream, uncompressed)
            .to_nix_string()
            .unwrap()
    })
    .await;

    let response = narinfo_text(&setup, HELLO_STORE_PATH_HASH).await;
    response.assert_status_ok();
    let served = parse_narinfo_body(&response.text()).expect("The response is a valid narinfo.");

    assert_eq!(served.compression, "none");
    assert_eq!(served.file_hash, None);
    assert_eq!(served.file_size, None);
    assert!(verify_narinfo_signature(
        &find_gradient_public_key_in_narinfo_sig_lines(&setup, &served),
        &served
    ));
    setup.db.finish().await;
}
