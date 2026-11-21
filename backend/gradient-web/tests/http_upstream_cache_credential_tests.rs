/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum_test::TestServer;
use gradient_entity::cache_upstream::UpstreamCredentialScheme;
use gradient_entity::{cache, cache_upstream, cache_user, ids::*, session, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows::{cache, cache_member, live_session, user};
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::{Value, json};

const PASSWORD: &str = "basic-password-must-stay-hidden";
const TOKEN: &str = "bearer-token-must-stay-hidden";
const PUBLIC_KEY: &str = "upstream-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("test step succeeds")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

struct Setup {
    db: TestDatabase,
    connection: DatabaseConnection,
    server: TestServer,
    auth: String,
    crypt: tempfile::NamedTempFile,
}

impl Setup {
    async fn new() -> Self {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        let session_id = SessionId::now_v7();
        gradient_db::rows::insert::<user::Entity, _>(&connection, user())
            .await
            .expect("test step succeeds");
        gradient_db::rows::insert::<session::Entity, _>(&connection, live_session(session_id))
            .await
            .expect("test step succeeds");
        gradient_db::rows::insert::<cache::Entity, _>(&connection, cache())
            .await
            .expect("test step succeeds");
        gradient_db::rows::insert::<cache_user::Entity, _>(
            &connection,
            cache_member(BASE_CACHE_ROLE_ADMIN_ID),
        )
        .await
        .expect("test step succeeds");
        let mut crypt = tempfile::NamedTempFile::new().expect("test step succeeds");
        crypt
            .write_all(b"test-secret-key-32-bytes-padding!")
            .expect("test step succeeds");
        let crypt_file = crypt.path().to_string_lossy().into_owned();
        let server = TestState::new()
            .web_db(connection.clone())
            .worker_db(connection.clone())
            .cache_db(connection.clone())
            .cli(move |cli| {
                cli.secrets.crypt_file = crypt_file;
                cli.outbound.allowed_non_public_network_targets = vec!["127.0.0.0/8".into()];
            })
            .server();
        Self {
            db,
            connection,
            server,
            auth: bearer(session_id),
            crypt,
        }
    }

    fn crypt_file(&self) -> String {
        self.crypt.path().to_string_lossy().into_owned()
    }

    fn upstreams_path() -> String {
        format!("/api/v1/caches/{}/upstream-caches", cache().name)
    }

    async fn add(&self, body: Value) -> axum_test::TestResponse {
        self.server
            .put(&Self::upstreams_path())
            .add_header("authorization", &self.auth)
            .json(&body)
            .await
    }

    async fn add_id(&self, body: Value) -> String {
        let response = self.add(body).await;
        response.assert_status_ok();
        response.json::<Value>()["message"]
            .as_str()
            .expect("test step succeeds")
            .to_owned()
    }

    async fn patch(&self, id: &str, body: Value) -> axum_test::TestResponse {
        self.server
            .patch(&format!("{}/{id}", Self::upstreams_path()))
            .add_header("authorization", &self.auth)
            .json(&body)
            .await
    }

    async fn stored_one(&self) -> cache_upstream::Model {
        let mut rows = self.stored().await;
        assert_eq!(rows.len(), 1);
        rows.remove(0)
    }

    fn decrypted(&self, row: &cache_upstream::Model) -> String {
        gradient_sources::decrypt_secret(
            &self.crypt_file(),
            row.encrypted_credential
                .as_deref()
                .expect("test step succeeds"),
        )
        .expect("test step succeeds")
        .expose()
        .to_owned()
    }

    async fn stored(&self) -> Vec<cache_upstream::Model> {
        cache_upstream::Entity::find()
            .filter(cache_upstream::Column::Cache.eq(cache().id))
            .all(&self.connection)
            .await
            .expect("test step succeeds")
    }
}

fn http_upstream(display_name: &str, url: &str, credential: Value) -> Value {
    json!({
        "type": "http",
        "display_name": display_name,
        "url": url,
        "public_key": PUBLIC_KEY,
        "credential": credential,
    })
}

#[tokio::test]
async fn adding_http_upstream_cache_with_credential_stores_it_encrypted_and_keeps_it_out_of_api_answers_and_logs()
 {
    let setup = Setup::new().await;
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    setup
        .add(http_upstream(
            "basic",
            "https://127.0.0.1:1/basic",
            json!({"scheme": "basic", "user": "alice", "password": PASSWORD}),
        ))
        .await
        .assert_status_ok();
    setup
        .add(http_upstream(
            "bearer",
            "https://127.0.0.1:1/bearer",
            json!({"scheme": "bearer", "token": TOKEN}),
        ))
        .await
        .assert_status_ok();

    let list = setup
        .server
        .get(&Setup::upstreams_path())
        .add_header("authorization", &setup.auth)
        .await;
    list.assert_status_ok();
    let text = list.text();
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
    assert!(!text.contains("encrypted_credential"), "{text}");
    let items: Value = list.json();
    let scheme_of = |name: &str| {
        items["message"]
            .as_array()
            .expect("test step succeeds")
            .iter()
            .find(|item| item["display_name"] == name)
            .map(|item| item["credential_scheme"].clone())
            .expect("test step succeeds")
    };
    assert_eq!(scheme_of("basic"), json!("basic"));
    assert_eq!(scheme_of("bearer"), json!("bearer"));

    let stored = setup.stored().await;
    let decrypt = |name: &str, scheme: UpstreamCredentialScheme| {
        let row = stored
            .iter()
            .find(|r| r.display_name == name)
            .expect("test step succeeds");
        assert_eq!(row.credential_scheme, Some(scheme));
        gradient_sources::decrypt_secret(
            &setup.crypt_file(),
            row.encrypted_credential
                .as_deref()
                .expect("test step succeeds"),
        )
        .expect("test step succeeds")
        .expose()
        .to_owned()
    };
    assert_eq!(
        decrypt("basic", UpstreamCredentialScheme::Basic),
        format!("alice:{PASSWORD}")
    );
    assert_eq!(decrypt("bearer", UpstreamCredentialScheme::Bearer), TOKEN);

    let logged = String::from_utf8(logs.0.lock().expect("test step succeeds").clone())
        .expect("test step succeeds");
    assert!(!logged.contains(PASSWORD), "{logged}");
    assert!(!logged.contains(TOKEN), "{logged}");
    setup.db.finish().await;
}

#[tokio::test]
async fn adding_http_upstream_cache_with_userinfo_or_query_in_url_is_refused_and_stores_no_row() {
    let setup = Setup::new().await;

    let userinfo = setup
        .add(http_upstream(
            "userinfo",
            "https://u:p@127.0.0.1:1/",
            Value::Null,
        ))
        .await;
    userinfo.assert_status_bad_request();
    assert!(
        userinfo.text().contains("`credential`"),
        "{}",
        userinfo.text()
    );
    assert!(!userinfo.text().contains("u:p@"), "{}", userinfo.text());

    let query = setup
        .add(http_upstream(
            "query",
            "https://127.0.0.1:1/?token=x",
            Value::Null,
        ))
        .await;
    query.assert_status_bad_request();
    assert!(query.text().contains("query"), "{}", query.text());
    assert!(!query.text().contains("token=x"), "{}", query.text());

    assert!(setup.stored().await.is_empty());
    setup.db.finish().await;
}

#[tokio::test]
async fn adding_http_upstream_cache_credential_with_http_url_or_empty_user_password_or_token_is_refused()
 {
    let setup = Setup::new().await;

    let cleartext = setup
        .add(http_upstream(
            "cleartext",
            "http://127.0.0.1:1/",
            json!({"scheme": "bearer", "token": TOKEN}),
        ))
        .await;
    cleartext.assert_status_bad_request();
    assert!(!cleartext.text().contains(TOKEN), "{}", cleartext.text());

    for credential in [
        json!({"scheme": "basic", "user": " ", "password": PASSWORD}),
        json!({"scheme": "bearer", "token": " "}),
    ] {
        setup
            .add(http_upstream("empty", "https://127.0.0.1:1/", credential))
            .await
            .assert_status_bad_request();
    }

    assert!(setup.stored().await.is_empty());
    setup.db.finish().await;
}

#[tokio::test]
async fn patch_http_upstream_cache_credential_sets_replaces_and_removes_it_without_returning_it() {
    let setup = Setup::new().await;
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let id = setup
        .add_id(http_upstream("plain", "https://127.0.0.1:1/", Value::Null))
        .await;

    let set = setup
        .patch(
            &id,
            json!({"credential": {"scheme": "bearer", "token": TOKEN}}),
        )
        .await;
    set.assert_status_ok();
    assert!(!set.text().contains(TOKEN), "{}", set.text());
    let row = setup.stored_one().await;
    assert_eq!(
        row.credential_scheme,
        Some(UpstreamCredentialScheme::Bearer)
    );
    assert_eq!(setup.decrypted(&row), TOKEN);

    setup
        .patch(
            &id,
            json!({"credential": {"scheme": "basic", "user": "alice", "password": PASSWORD}}),
        )
        .await
        .assert_status_ok();
    let row = setup.stored_one().await;
    assert_eq!(row.credential_scheme, Some(UpstreamCredentialScheme::Basic));
    assert_eq!(setup.decrypted(&row), format!("alice:{PASSWORD}"));

    setup
        .patch(&id, json!({"credential": {"scheme": "none"}}))
        .await
        .assert_status_ok();
    let row = setup.stored_one().await;
    assert_eq!(row.credential_scheme, None);
    assert_eq!(row.encrypted_credential, None);

    let logged = String::from_utf8(logs.0.lock().expect("test step succeeds").clone())
        .expect("test step succeeds");
    assert!(!logged.contains(PASSWORD), "{logged}");
    assert!(!logged.contains(TOKEN), "{logged}");
    setup.db.finish().await;
}

#[tokio::test]
async fn patch_upstream_cache_url_to_http_or_another_host_without_resending_stored_credential_is_refused()
 {
    let setup = Setup::new().await;
    let id = setup
        .add_id(http_upstream(
            "bearer",
            "https://127.0.0.1:1/",
            json!({"scheme": "bearer", "token": TOKEN}),
        ))
        .await;

    let cleartext = setup
        .patch(&id, json!({"url": "http://127.0.0.1:1/"}))
        .await;
    cleartext.assert_status_bad_request();
    assert!(!cleartext.text().contains(TOKEN), "{}", cleartext.text());

    setup
        .patch(&id, json!({"url": "https://127.0.0.2:1/"}))
        .await
        .assert_status_bad_request();
    assert_eq!(
        setup.stored_one().await.url.as_deref(),
        Some("https://127.0.0.1:1")
    );

    setup
        .patch(&id, json!({"url": "https://127.0.0.1:1/other"}))
        .await
        .assert_status_ok();
    setup
        .patch(
            &id,
            json!({"url": "https://127.0.0.2:1/", "credential": {"scheme": "bearer", "token": TOKEN}}),
        )
        .await
        .assert_status_ok();
    setup
        .patch(
            &id,
            json!({"url": "http://127.0.0.1:1/", "credential": {"scheme": "none"}}),
        )
        .await
        .assert_status_ok();
    let row = setup.stored_one().await;
    assert_eq!(row.url.as_deref(), Some("http://127.0.0.1:1"));
    assert_eq!(row.encrypted_credential, None);
    setup.db.finish().await;
}

#[tokio::test]
async fn patch_with_active_refused_for_upstream_cache_with_query_string_in_url() {
    let setup = Setup::new().await;
    let id = CacheUpstreamId::now_v7();
    gradient_db::rows::insert::<cache_upstream::Entity, _>(
        &setup.connection,
        cache_upstream::Model {
            id,
            cache: cache().id,
            display_name: "query".into(),
            kind: cache_upstream::CacheUpstreamKind::Http,
            url: Some("https://127.0.0.1:1/?token=x".into()),
            public_key: Some(PUBLIC_KEY.into()),
            active: false,
            ..Default::default()
        },
    )
    .await
    .expect("test step succeeds");

    let reactivate = setup.patch(&id.to_string(), json!({"active": true})).await;
    reactivate.assert_status_bad_request();
    assert!(
        !reactivate.text().contains("token=x"),
        "{}",
        reactivate.text()
    );
    assert!(!setup.stored_one().await.active);
    setup.db.finish().await;
}

async fn insert_inactive_upstream_cache_row_with_stored_url(
    setup: &Setup,
    kind: cache_upstream::CacheUpstreamKind,
    url: &str,
) -> CacheUpstreamId {
    let id = CacheUpstreamId::now_v7();
    let is_proto = kind == cache_upstream::CacheUpstreamKind::GradientProto;
    gradient_db::rows::insert::<cache_upstream::Entity, _>(
        &setup.connection,
        cache_upstream::Model {
            id,
            cache: cache().id,
            display_name: format!("{kind:?}"),
            kind,
            url: Some(url.into()),
            public_key: (!is_proto).then(|| PUBLIC_KEY.into()),
            remote_cache_name: is_proto.then(|| "remote".into()),
            active: false,
            ..Default::default()
        },
    )
    .await
    .expect("test step succeeds");
    id
}

#[tokio::test]
async fn list_api_flags_inactive_upstream_caches_and_removes_credentials_and_query_from_url() {
    let setup = Setup::new().await;
    insert_inactive_upstream_cache_row_with_stored_url(
        &setup,
        cache_upstream::CacheUpstreamKind::Http,
        "https://127.0.0.1:1/prefix?token=x",
    )
    .await;
    insert_inactive_upstream_cache_row_with_stored_url(
        &setup,
        cache_upstream::CacheUpstreamKind::GradientProto,
        &format!("https://alice:{PASSWORD}@127.0.0.1:2"),
    )
    .await;

    let listed = setup
        .server
        .get(&Setup::upstreams_path())
        .add_header("authorization", &setup.auth)
        .await;
    listed.assert_status_ok();
    let text = listed.text();
    assert!(!text.contains("token=x"), "{text}");
    assert!(!text.contains("alice"), "{text}");
    assert!(!text.contains(PASSWORD), "{text}");
    let items = listed.json::<Value>()["message"]
        .as_array()
        .expect("test step succeeds")
        .clone();
    let mut urls: Vec<&str> = items
        .iter()
        .map(|item| {
            assert_eq!(item["active"], false);
            assert_eq!(item["stored_url_has_user_password_or_query"], true);
            item["url"].as_str().expect("test step succeeds")
        })
        .collect();
    urls.sort_unstable();
    assert_eq!(urls, ["https://127.0.0.1:1/prefix", "https://127.0.0.1:2/"]);
    setup.db.finish().await;
}

#[tokio::test]
async fn patch_active_true_with_url_is_refused_for_gradient_proto_upstream_cache_with_user_name_or_password_in_stored_url()
 {
    let setup = Setup::new().await;
    let id = insert_inactive_upstream_cache_row_with_stored_url(
        &setup,
        cache_upstream::CacheUpstreamKind::GradientProto,
        &format!("https://alice:{PASSWORD}@127.0.0.1:1"),
    )
    .await;

    let reactivate = setup
        .patch(
            &id.to_string(),
            json!({"active": true, "url": "https://127.0.0.1:1"}),
        )
        .await;
    reactivate.assert_status_bad_request();
    assert!(
        !reactivate.text().contains(PASSWORD),
        "{}",
        reactivate.text()
    );
    assert!(!setup.stored_one().await.active);
    setup.db.finish().await;
}

#[tokio::test]
async fn patch_upstream_cache_url_or_credential_clears_persisted_nar_download_urls() {
    use gradient_entity::{derivation, derivation_output};
    use sea_orm::ActiveValue::Set;

    let setup = Setup::new().await;
    setup
        .add_id(http_upstream("plain", "https://127.0.0.1:1/", Value::Null))
        .await;
    let upstream = setup.stored_one().await.id;
    let derivation_id = DerivationId::now_v7();
    gradient_db::rows::insert::<derivation::Entity, _>(
        &setup.connection,
        gradient_fixtures::rows::derivation_named(derivation_id, "x"),
    )
    .await
    .expect("test step succeeds");
    let output = gradient_db::rows::insert::<derivation_output::Entity, _>(
        &setup.connection,
        gradient_fixtures::rows::derivation_output(derivation_id, "a4a4", Some(5)),
    )
    .await
    .expect("test step succeeds");
    let persist = || async {
        gradient_db::rows::update::<derivation_output::Entity, _>(
            &setup.connection,
            derivation_output::ActiveModel {
                id: Set(output.id),
                external_url: Set(Some("https://127.0.0.1:1/nar/a4a4.nar".into())),
                verified_by_upstream: Set(Some(upstream)),
                ..Default::default()
            },
        )
        .await
        .expect("test step succeeds");
    };
    let persisted = || async {
        let row = setup
            .db
            .find::<derivation_output::Entity>(output.id)
            .await
            .expect("output row exists");
        (
            row.external_url.is_some(),
            row.verified_by_upstream.is_some(),
        )
    };

    persist().await;
    setup
        .patch(&upstream.to_string(), json!({"display_name": "renamed"}))
        .await
        .assert_status_ok();
    assert_eq!(persisted().await, (true, true), "a rename keeps them");

    setup
        .patch(
            &upstream.to_string(),
            json!({"credential": {"scheme": "bearer", "token": TOKEN}}),
        )
        .await
        .assert_status_ok();
    assert_eq!(persisted().await, (false, false), "a new credential");

    persist().await;
    setup
        .patch(
            &upstream.to_string(),
            json!({"url": "https://127.0.0.1:1/other"}),
        )
        .await
        .assert_status_ok();
    assert_eq!(persisted().await, (false, false), "a new URL");
    setup.db.finish().await;
}

#[tokio::test]
async fn adding_upstream_cache_beyond_per_cache_limit_is_refused_with_403_and_no_row_is_stored() {
    let setup = Setup::new().await;
    for _ in 0..gradient_db::caches::upstream::MAX_UPSTREAM_CACHE_COUNT {
        insert_inactive_upstream_cache_row_with_stored_url(
            &setup,
            cache_upstream::CacheUpstreamKind::Http,
            "https://127.0.0.1:1/full",
        )
        .await;
    }

    let refused = setup
        .add(http_upstream(
            "one too many",
            "https://127.0.0.1:1/more",
            json!({"scheme": "bearer", "token": TOKEN}),
        ))
        .await;

    refused.assert_status_forbidden();
    assert_eq!(
        setup.stored().await.len(),
        gradient_db::caches::upstream::MAX_UPSTREAM_CACHE_COUNT
    );
    setup.db.finish().await;
}
