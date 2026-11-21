/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::cache_upstream::{
    ActiveModel as ACacheUpstream, CacheUpstreamKind, Column as CCacheUpstream,
    Entity as ECacheUpstream, Model as MCacheUpstream, UpstreamCredentialScheme,
};
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use percent_encoding::percent_decode_str;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QuerySelect, TransactionTrait,
};

struct SplitUpstreamUrlCredentials {
    url: ValidatedHttpBaseUrl,
    basic_payload: String,
}

fn split_url_credentials_into_basic_auth(raw: &str) -> Option<SplitUpstreamUrlCredentials> {
    let mut parsed = url::Url::parse(raw.trim()).ok()?;
    let user = percent_decode_str(parsed.username()).decode_utf8().ok()?;
    let password = percent_decode_str(parsed.password().unwrap_or_default())
        .decode_utf8()
        .ok()?;
    let basic_payload = format!("{user}:{password}");
    parsed.set_username("").ok()?;
    parsed.set_password(None).ok()?;
    let url = parsed.as_str().parse().ok()?;
    Some(SplitUpstreamUrlCredentials { url, basic_payload })
}

/// Call this function at startup. Don't call it from a migration, because migrations can't read the encryption key file.
pub async fn move_userinfo_of_stored_upstream_urls_into_encrypted_credentials(
    db: &DatabaseConnection,
    crypt_file: &str,
) -> anyhow::Result<()> {
    let candidates = ECacheUpstream::find()
        .filter(upstream_cache_url_contains_at_sign_or_question_mark_condition())
        .all(db)
        .await?;
    for candidate in candidates {
        let txn = db.begin().await?;
        let Some(row) = ECacheUpstream::find_by_id(candidate.id)
            .filter(upstream_cache_url_contains_at_sign_or_question_mark_condition())
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            continue;
        };
        let Some(raw) = row.url.as_deref() else {
            continue;
        };
        let Ok(parsed) = url::Url::parse(raw.trim()) else {
            continue;
        };
        let has_userinfo = !parsed.username().is_empty() || parsed.password().is_some();
        if !has_userinfo && parsed.query().is_none() {
            continue;
        }
        let movable = row.kind == CacheUpstreamKind::Http
            && row.encrypted_credential.is_none()
            && parsed.query().is_none();
        match movable
            .then(|| split_url_credentials_into_basic_auth(raw))
            .flatten()
        {
            Some(moved) => {
                encrypt_upstream_url_basic_auth_and_replace_url_without_credentials(
                    &txn, crypt_file, &row, moved,
                )
                .await?
            }
            None if row.active => {
                gradient_db::rows::update::<ECacheUpstream, _>(
                    &txn,
                    ACacheUpstream {
                        id: Set(row.id),
                        active: Set(false),
                        ..Default::default()
                    },
                )
                .await?;
                tracing::warn!(upstream = %row.id, "Upstream cache is now inactive. The stored URL has a query, or the user and password can't move to credentials. Fix the URL, then set the upstream active again.");
            }
            None => {}
        }
        gradient_db::caches::upstream::clear_nar_url_and_upstream_reference_for_build_outputs_verified_by_upstream(&txn, row.id).await?;
        txn.commit().await?;
    }
    Ok(())
}

fn upstream_cache_url_contains_at_sign_or_question_mark_condition() -> Condition {
    Condition::any()
        .add(CCacheUpstream::Url.like("%@%"))
        .add(CCacheUpstream::Url.like("%?%"))
}

async fn encrypt_upstream_url_basic_auth_and_replace_url_without_credentials<C: ConnectionTrait>(
    db: &C,
    crypt_file: &str,
    row: &MCacheUpstream,
    moved: SplitUpstreamUrlCredentials,
) -> anyhow::Result<()> {
    let encrypted = gradient_sources::encrypt_secret(crypt_file, &moved.basic_payload)?;
    gradient_db::rows::update::<ECacheUpstream, _>(
        db,
        ACacheUpstream {
            id: Set(row.id),
            url: Set(Some(moved.url.as_str().to_owned())),
            encrypted_credential: Set(Some(encrypted)),
            credential_scheme: Set(Some(UpstreamCredentialScheme::Basic)),
            ..Default::default()
        },
    )
    .await?;
    if moved.url.as_str().starts_with("http://") {
        tracing::warn!(upstream = %row.id, "Credentials for the upstream cache travel over unencrypted http. Change the URL to https.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::{Arc, Mutex};

    use gradient_entity::cache_upstream::{
        CacheUpstreamKind, Entity as ECacheUpstream, Model as MCacheUpstream,
        UpstreamCredentialScheme,
    };
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_types::ids::CacheUpstreamId;
    use sea_orm::DatabaseConnection;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::move_userinfo_of_stored_upstream_urls_into_encrypted_credentials;
    use crate::upstream_source::{fetch_from_upstream_caches, substitution_sources};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
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

    struct Fixture {
        db: TestDatabase,
        connection: DatabaseConnection,
        crypt: tempfile::NamedTempFile,
    }

    impl Fixture {
        async fn new() -> Self {
            let db = TestDatabase::create().await;
            let connection = db.connection();
            gradient_db::rows::insert::<gradient_entity::user::Entity, _>(
                &connection,
                rows::user(),
            )
            .await
            .unwrap();
            gradient_db::rows::insert::<gradient_entity::cache::Entity, _>(
                &connection,
                rows::cache(),
            )
            .await
            .unwrap();
            Self::from_prepared_test_database(db)
        }

        fn from_prepared_test_database(db: TestDatabase) -> Self {
            let mut crypt = tempfile::NamedTempFile::new().unwrap();
            crypt
                .write_all(b"test-secret-key-32-bytes-padding!")
                .unwrap();
            Self {
                connection: db.connection(),
                db,
                crypt,
            }
        }

        fn crypt_file(&self) -> String {
            self.crypt.path().to_string_lossy().into_owned()
        }

        async fn insert(&self, kind: CacheUpstreamKind, url: &str) -> MCacheUpstream {
            gradient_db::rows::insert::<ECacheUpstream, _>(
                &self.connection,
                MCacheUpstream {
                    kind,
                    ..rows::external_upstream(
                        CacheUpstreamId::now_v7(),
                        rows::cache().id,
                        url,
                        "upstream.example-1:key",
                    )
                },
            )
            .await
            .unwrap()
        }

        async fn run(&self) -> String {
            let logs = CapturedLogs::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_ansi(false)
                .finish();
            let _guard = tracing::subscriber::set_default(subscriber);
            move_userinfo_of_stored_upstream_urls_into_encrypted_credentials(
                &self.connection,
                &self.crypt_file(),
            )
            .await
            .unwrap();
            String::from_utf8(logs.0.lock().unwrap().clone()).unwrap()
        }

        async fn reload(&self, row: &MCacheUpstream) -> MCacheUpstream {
            self.db
                .find::<ECacheUpstream>(row.id)
                .await
                .expect("upstream row exists")
        }

        fn payload(&self, row: &MCacheUpstream) -> String {
            gradient_sources::decrypt_secret(
                &self.crypt_file(),
                row.encrypted_credential.as_deref().expect("credential set"),
            )
            .unwrap()
            .expose()
            .to_owned()
        }
    }

    #[tokio::test]
    async fn test_percent_encoded_url_credentials_move_to_encrypted_basic_credential() {
        let fixture = Fixture::new().await;
        let row = fixture
            .insert(CacheUpstreamKind::Http, "https://u%40x:p%3Aw@h.example/")
            .await;

        fixture.run().await;

        let moved = fixture.reload(&row).await;
        assert_eq!(moved.url.as_deref(), Some("https://h.example"));
        assert_eq!(
            moved.credential_scheme,
            Some(UpstreamCredentialScheme::Basic)
        );
        assert_eq!(fixture.payload(&moved), "u@x:p:w");
        assert!(moved.active);
        fixture.db.finish().await;
    }

    #[tokio::test]
    async fn stored_upstream_cache_url_with_username_and_no_password_moves_to_basic_credential_with_empty_password()
     {
        let fixture = Fixture::new().await;
        let row = fixture
            .insert(CacheUpstreamKind::Http, "https://builder@h.example/prefix")
            .await;

        fixture.run().await;

        let moved = fixture.reload(&row).await;
        assert_eq!(moved.url.as_deref(), Some("https://h.example/prefix"));
        assert_eq!(fixture.payload(&moved), "builder:");
        fixture.db.finish().await;
    }

    #[tokio::test]
    async fn startup_sets_http_url_with_query_string_and_proto_url_with_user_and_password_inactive_without_changing_urls()
     {
        let fixture = Fixture::new().await;
        let query = fixture
            .insert(CacheUpstreamKind::Http, "https://h.example/?token=x")
            .await;
        let proto = fixture
            .insert(
                CacheUpstreamKind::GradientProto,
                "https://u:p@remote.example",
            )
            .await;

        let logs = fixture.run().await;

        assert!(logs.contains(&query.id.to_string()), "{logs}");
        assert!(!logs.contains("token=x") && !logs.contains("u:p"), "{logs}");
        for row in [query, proto] {
            let after = fixture.reload(&row).await;
            assert!(!after.active, "{:?}", after.url);
            assert_eq!(after.url, row.url);
            assert_eq!(after.encrypted_credential, None);
        }
        fixture.db.finish().await;
    }

    #[tokio::test]
    async fn moving_url_credentials_twice_changes_no_upstream_cache_row() {
        let fixture = Fixture::new().await;
        fixture
            .insert(CacheUpstreamKind::Http, "https://u:p@h.example")
            .await;
        fixture
            .insert(CacheUpstreamKind::Http, "https://h.example/?token=x")
            .await;
        fixture
            .insert(CacheUpstreamKind::Http, "https://plain.example")
            .await;
        fixture.run().await;
        let first = fixture.db.rows::<ECacheUpstream>().await;

        fixture.run().await;

        let mut second = fixture.db.rows::<ECacheUpstream>().await;
        let mut first = first;
        first.sort_by_key(|row| row.id);
        second.sort_by_key(|row| row.id);
        assert_eq!(first, second);
        assert!(first.iter().any(|row| row.encrypted_credential.is_some()));
        fixture.db.finish().await;
    }

    #[tokio::test]
    async fn fetch_after_startup_moves_url_credentials_sends_moved_credential_to_upstream_server() {
        let fixture = Fixture::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nar/x.nar"))
            .and(header("authorization", "Basic YnVpbGRlcjpwYTpzcw=="))
            .respond_with(ResponseTemplate::new(200).set_body_string("nar bytes"))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .with_priority(2)
            .mount(&server)
            .await;
        let with_userinfo = server.uri().replacen("://", "://builder:pa%3Ass@", 1);
        let row = fixture
            .insert(CacheUpstreamKind::Http, &with_userinfo)
            .await;

        let logs = fixture.run().await;

        assert!(logs.contains(&row.id.to_string()), "{logs}");
        assert!(
            !logs.contains("pa%3Ass") && !logs.contains("pa:ss"),
            "{logs}"
        );
        let moved = fixture.reload(&row).await;
        assert_eq!(moved.url.as_deref(), Some(server.uri().as_str()));
        let sources = substitution_sources(&[moved], &fixture.crypt_file()).await;
        assert_eq!(sources.len(), 1);
        let http = crate::upstream::http_client_for_user_supplied_urls_allowing_loopback();
        let object = fetch_from_upstream_caches(&http, &sources, "nar/x.nar", None).await;
        assert!(object.is_some(), "the moved credential is sent");
        fixture.db.finish().await;
    }

    #[tokio::test]
    async fn startup_moving_upstream_cache_url_to_encrypted_credentials_clears_nar_download_urls_of_moved_or_inactive_caches_and_keeps_untouched_cache_urls()
     {
        use gradient_entity::derivation_output::{
            Column as CDerivationOutput, Entity as EDerivationOutput,
        };
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::{CacheId, EvaluationId};
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        const MOVED_OUT: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
        const QUERY_OUT: &str = "b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";
        const OPEN_OUT: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
        let (moved, query, open) = (
            CacheUpstreamId::now_v7(),
            CacheUpstreamId::now_v7(),
            CacheUpstreamId::now_v7(),
        );
        let mut builder = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation("amig98arvp03zydc9klqsvmmrkrwhnpc-moved.drv")
            .cache_available()
            .output(MOVED_OUT)
            .verified_by(moved)
            .derivation("2ivlx3wmzxa05x1hxga3iv7yx33p86cs-query.drv")
            .cache_available()
            .output(QUERY_OUT)
            .verified_by(query)
            .derivation("x4ygkdbrp0g5s93z10m59v7g0vy1hw28-open.drv")
            .cache_available()
            .output(OPEN_OUT)
            .verified_by(open)
            .into_cache(CacheId::now_v7());
        for (id, url) in [
            (moved, "https://u:p@h.example"),
            (query, "https://q.example/?token=x"),
            (open, "https://open.example"),
        ] {
            builder = builder.upstream(
                id,
                url,
                "upstream.example-1:key",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadOnly,
                true,
            );
        }
        let fixture = Fixture::from_prepared_test_database(builder.into_database().await);
        let persisted = [
            (MOVED_OUT, "https://u:p@h.example/nar/moved.nar"),
            (QUERY_OUT, "https://q.example/?token=x/nar/query.nar"),
            (OPEN_OUT, "https://open.example/nar/open.nar"),
        ];
        for (hash, url) in persisted {
            EDerivationOutput::update_many()
                .col_expr(CDerivationOutput::ExternalUrl, Expr::value(url))
                .filter(CDerivationOutput::Hash.eq(hash))
                .exec(&fixture.connection)
                .await
                .unwrap();
        }

        fixture.run().await;

        let mut outputs: Vec<_> = fixture
            .db
            .rows::<EDerivationOutput>()
            .await
            .into_iter()
            .map(|o| (o.hash, o.external_url, o.verified_by_upstream))
            .collect();
        outputs.sort();
        assert_eq!(
            outputs,
            [
                (Some(MOVED_OUT.to_owned()), None, None),
                (Some(QUERY_OUT.to_owned()), None, None),
                (
                    Some(OPEN_OUT.to_owned()),
                    Some("https://open.example/nar/open.nar".to_owned()),
                    Some(open),
                ),
            ]
        );
        let derivation_outputs_with_nar_url_still_offered_to_workers =
            gradient_db::caches::upstream::find_build_outputs_by_hashes_with_nar_urls_from_upstream_caches_workers_may_use(
                &fixture.connection,
                [MOVED_OUT, QUERY_OUT, OPEN_OUT].map(str::to_owned).to_vec(),
            )
            .await
            .unwrap();
        let derivation_outputs_with_nar_url_still_offered_to_workers: Vec<_> =
            derivation_outputs_with_nar_url_still_offered_to_workers
                .into_iter()
                .filter_map(|o| o.external_url)
                .collect();
        assert_eq!(
            derivation_outputs_with_nar_url_still_offered_to_workers,
            ["https://open.example/nar/open.nar"]
        );
        fixture.db.finish().await;
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "Tests only read `pg_stat_activity`. The check of query plans doesn't cover this read."
    )]
    async fn current_database_has_transaction_waiting_for_lock(
        connection: &DatabaseConnection,
    ) -> bool {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        connection
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock') AS waiting",
            ))
            .await
            .unwrap()
            .expect("one row")
            .try_get::<bool>("", "waiting")
            .unwrap()
    }

    #[tokio::test]
    async fn startup_move_does_not_overwrite_credential_and_url_saved_while_waiting_for_row_lock() {
        use gradient_entity::cache_upstream::ActiveModel as ACacheUpstream;
        use sea_orm::{ActiveValue::Set, EntityTrait, QuerySelect, TransactionTrait};

        let fixture = Fixture::new().await;
        let row = fixture
            .insert(CacheUpstreamKind::Http, "https://old:secret@h.example/")
            .await;
        let patch = fixture.connection.begin().await.unwrap();
        ECacheUpstream::find_by_id(row.id)
            .lock_exclusive()
            .one(&patch)
            .await
            .unwrap();
        let crypt_file = fixture.crypt_file();
        let move_url_credentials_into_encrypted_credentials = async {
            move_userinfo_of_stored_upstream_urls_into_encrypted_credentials(
                &fixture.connection,
                &crypt_file,
            )
            .await
            .unwrap();
        };
        let another_server_saves_and_commits_new_credential_and_url_after_move_blocks_on_row_lock = async {
            while !current_database_has_transaction_waiting_for_lock(&fixture.connection).await {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let saved = gradient_sources::encrypt_secret(&crypt_file, "new:token").unwrap();
            gradient_db::rows::update::<ECacheUpstream, _>(
                &patch,
                ACacheUpstream {
                    id: Set(row.id),
                    url: Set(Some("https://h.example".into())),
                    encrypted_credential: Set(Some(saved)),
                    credential_scheme: Set(Some(UpstreamCredentialScheme::Basic)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            patch.commit().await.unwrap();
        };

        tokio::join!(
            move_url_credentials_into_encrypted_credentials,
            another_server_saves_and_commits_new_credential_and_url_after_move_blocks_on_row_lock
        );

        let after = fixture.reload(&row).await;
        assert_eq!(after.url.as_deref(), Some("https://h.example"));
        assert_eq!(fixture.payload(&after), "new:token");
        fixture.db.finish().await;
    }
}
