/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::atomic::Ordering;

use axum_test::TestServer;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, cache_upstream, cache_user, ids::*, session, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows::{cache, cache_member, live_session, user};
use gradient_test_support::state::TestState;
use gradient_test_support::web::{bearer, spawn_loopback_http_server_with_connection_counter};
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

const SECRET_MARKER: &str = "SECRET-UPSTREAM-BODY";
const UPSTREAM_RESPONSE_BODY: &str = "StoreDir: /nix/store\nSECRET-UPSTREAM-BODY\n";
const DRV: &str = "0123456789abcdfghijklmnpqrsvwxyz-hello.drv";

struct HttpUpstreamCacheTestSetup {
    db: TestDatabase,
    server: TestServer,
    auth: String,
    upstream: CacheUpstreamId,
}

async fn create_test_database_with_admin_user_and_start_web_server_for_http_upstream_cache(
    upstream_url: &str,
    allowed_non_public_network_targets: Vec<String>,
) -> HttpUpstreamCacheTestSetup {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    let session_id = SessionId::now_v7();
    gradient_db::rows::insert::<user::Entity, _>(&conn, user())
        .await
        .expect("insert user");
    gradient_db::rows::insert::<session::Entity, _>(&conn, live_session(session_id))
        .await
        .expect("insert session");
    gradient_db::rows::insert::<cache::Entity, _>(
        &conn,
        cache::Model {
            public: true,
            pull_through: true,
            ..cache()
        },
    )
    .await
    .expect("insert cache");
    gradient_db::rows::insert::<cache_user::Entity, _>(
        &conn,
        cache_member(BASE_CACHE_ROLE_ADMIN_ID),
    )
    .await
    .expect("insert cache admin");
    let upstream = gradient_db::rows::insert::<cache_upstream::Entity, _>(
        &conn,
        cache_upstream::Model {
            id: CacheUpstreamId::now_v7(),
            cache: cache().id,
            display_name: "internal".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            kind: CacheUpstreamKind::Http,
            url: Some(upstream_url.into()),
            public_key: Some("internal-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=".into()),
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
            cli.outbound.allowed_non_public_network_targets = allowed_non_public_network_targets
        })
        .server();
    HttpUpstreamCacheTestSetup {
        db,
        server,
        auth: bearer(session_id),
        upstream: upstream.id,
    }
}

async fn fetch_upstream_cache_test_endpoint_bodies(setup: &HttpUpstreamCacheTestSetup) -> String {
    let cache_name = cache().name;
    let nar = setup
        .server
        .get(&format!(
            "/cache/{cache_name}/nar/upstream/{}/nar/abc.nar",
            setup.upstream
        ))
        .await;
    let log = setup
        .server
        .get(&format!("/cache/{cache_name}/log/{DRV}"))
        .await;
    let test = setup
        .server
        .post(&format!(
            "/api/v1/caches/{cache_name}/upstream-caches/{}/test",
            setup.upstream
        ))
        .add_header("authorization", &setup.auth)
        .await;
    test.assert_status_ok();
    [nar.text(), log.text(), test.text()].join("\n")
}

#[tokio::test]
async fn empty_admin_allow_list_makes_nar_download_build_log_and_upstream_cache_test_endpoints_open_no_loopback_upstream_cache_connection_or_return_response_body()
 {
    let (base_url, connections) =
        spawn_loopback_http_server_with_connection_counter(UPSTREAM_RESPONSE_BODY);
    let setup = create_test_database_with_admin_user_and_start_web_server_for_http_upstream_cache(
        &base_url,
        Vec::new(),
    )
    .await;

    let bodies = fetch_upstream_cache_test_endpoint_bodies(&setup).await;

    assert_eq!(connections.load(Ordering::SeqCst), 0);
    assert!(!bodies.contains(SECRET_MARKER), "{bodies}");
    assert!(!bodies.contains("StoreDir"), "{bodies}");
    setup.db.finish().await;
}

#[tokio::test]
async fn admin_allow_list_with_loopback_range_lets_upstream_cache_endpoints_connect() {
    let (base_url, connections) =
        spawn_loopback_http_server_with_connection_counter(UPSTREAM_RESPONSE_BODY);
    let setup = create_test_database_with_admin_user_and_start_web_server_for_http_upstream_cache(
        &base_url,
        vec!["127.0.0.0/8".into()],
    )
    .await;

    let bodies = fetch_upstream_cache_test_endpoint_bodies(&setup).await;

    assert!(connections.load(Ordering::SeqCst) >= 3, "{bodies}");
    assert!(bodies.contains(SECRET_MARKER), "{bodies}");
    setup.db.finish().await;
}

async fn start_web_server_with_allowlist_send_create_upstream_cache_request_and_return_status_and_stored_urls_for_kind(
    allowed_non_public_network_targets: Vec<String>,
    kind: CacheUpstreamKind,
    body: serde_json::Value,
) -> (axum::http::StatusCode, String, Vec<Option<String>>) {
    let setup = create_test_database_with_admin_user_and_start_web_server_for_http_upstream_cache(
        "https://cache.example",
        allowed_non_public_network_targets,
    )
    .await;
    let response = setup
        .server
        .put(&format!("/api/v1/caches/{}/upstream-caches", cache().name))
        .add_header("authorization", &setup.auth)
        .json(&body)
        .await;
    let stored = cache_upstream::Entity::find()
        .filter(cache_upstream::Column::Kind.eq(kind))
        .filter(cache_upstream::Column::Id.ne(setup.upstream))
        .all(&setup.db.connection())
        .await
        .expect("list upstream caches")
        .into_iter()
        .map(|row| row.url)
        .collect();
    setup.db.finish().await;
    (response.status_code(), response.text(), stored)
}

fn gradient_proto_upstream_cache_create_request_json_body(url: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "gradient_proto",
        "url": url,
        "remote_cache": "prod",
        "display_name": "remote",
    })
}

#[tokio::test]
async fn empty_admin_allow_list_refuses_a_gradient_proto_upstream_whose_host_name_resolves_to_loopback()
 {
    let (status, _, stored) =
        start_web_server_with_allowlist_send_create_upstream_cache_request_and_return_status_and_stored_urls_for_kind(
            Vec::new(),
            CacheUpstreamKind::GradientProto,
            gradient_proto_upstream_cache_create_request_json_body("http://localhost.:1/"),
        )
        .await;
    assert!(status.is_client_error(), "{status}");
    assert!(stored.is_empty(), "{stored:?}");
}

#[tokio::test]
async fn empty_admin_allow_list_refuses_http_upstream_cache_whose_host_name_resolves_to_loopback() {
    let (status, body, stored) =
        start_web_server_with_allowlist_send_create_upstream_cache_request_and_return_status_and_stored_urls_for_kind(
            Vec::new(),
            CacheUpstreamKind::Http,
            serde_json::json!({
                "type": "http",
                "url": "http://localhost.:1/",
                "public_key": "internal-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=",
                "display_name": "loopback",
            }),
        )
        .await;
    assert!(status.is_client_error(), "{status}");
    assert!(
        !body.contains("127.0.0.1") && !body.contains("::1"),
        "{body}"
    );
    assert!(stored.is_empty(), "{stored:?}");
}

fn does_localhost_with_trailing_dot_resolve() -> bool {
    use std::net::ToSocketAddrs;
    ("localhost.", 0)
        .to_socket_addrs()
        .is_ok_and(|mut addresses| addresses.next().is_some())
}

#[tokio::test]
async fn control_case_admin_allow_list_with_loopback_range_stores_the_normalised_gradient_proto_upstream_url()
 {
    if !does_localhost_with_trailing_dot_resolve() {
        eprintln!("skipped: hostname localhost. cannot resolve on this host");
        return;
    }
    let (status, _, stored) =
        start_web_server_with_allowlist_send_create_upstream_cache_request_and_return_status_and_stored_urls_for_kind(
            vec!["127.0.0.0/8".into()],
            CacheUpstreamKind::GradientProto,
            gradient_proto_upstream_cache_create_request_json_body("http://LOCALHOST.:1/"),
        )
        .await;
    assert!(status.is_success(), "{status}");
    assert_eq!(stored, vec![Some("http://localhost.:1".to_owned())]);
}

#[tokio::test]
async fn empty_admin_allow_list_refuses_patch_http_upstream_cache_url_to_loopback_with_same_status_and_body_as_create()
 {
    let setup = create_test_database_with_admin_user_and_start_web_server_for_http_upstream_cache(
        "https://cache.example",
        Vec::new(),
    )
    .await;
    let upstream_caches_rest_api_path = format!("/api/v1/caches/{}/upstream-caches", cache().name);
    for loopback_url in ["http://localhost.:1/", "http://127.0.0.1:1/"] {
        let create = setup
            .server
            .put(&upstream_caches_rest_api_path)
            .add_header("authorization", &setup.auth)
            .json(&serde_json::json!({
                "type": "http",
                "url": loopback_url,
                "public_key": "internal-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=",
                "display_name": "loopback",
            }))
            .await;
        let patch = setup
            .server
            .patch(&format!(
                "{upstream_caches_rest_api_path}/{}",
                setup.upstream
            ))
            .add_header("authorization", &setup.auth)
            .json(&serde_json::json!({ "url": loopback_url }))
            .await;
        assert!(
            patch.status_code().is_client_error(),
            "{loopback_url}: {}",
            patch.status_code()
        );
        assert_eq!(patch.status_code(), create.status_code(), "{loopback_url}");
        assert_eq!(patch.text(), create.text(), "{loopback_url}");
    }
    let stored = cache_upstream::Entity::find_by_id(setup.upstream)
        .one(&setup.db.connection())
        .await
        .expect("read upstream cache")
        .expect("upstream cache exists");
    assert_eq!(stored.url.as_deref(), Some("https://cache.example"));
    setup.db.finish().await;
}
