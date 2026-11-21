/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::actions::{decrypt_secret_with_file, encrypt_secret_with_file};
use anyhow::{Context, Result};
use chrono::{Duration, NaiveDateTime};
use gradient_db::git_host_connection_repository_grants::GitHostConnectionRepositoryAccessProof;
use gradient_git_host::gitea_oauth::get_new_tokens_from_git_host_with_refresh_token;
use gradient_types::*;
use gradient_util::http_base_url::{
    url_origin_without_credentials, urls_differ_in_scheme_host_or_port,
};
use sea_orm::sea_query::LockType;
use sea_orm::{EntityTrait, QuerySelect, TransactionSession, TransactionTrait};

pub fn access_token_expires_at_from_lifetime_seconds(
    now: NaiveDateTime,
    lifetime_seconds: Option<i64>,
) -> Option<NaiveDateTime> {
    lifetime_seconds.map(|seconds| now + Duration::seconds(seconds))
}

fn git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute(
    connection: &MGitHostConnection,
    now: NaiveDateTime,
) -> bool {
    connection.encrypted_refresh_token.is_some()
        && connection
            .access_token_expires_at
            .is_some_and(|expires_at| expires_at <= now + Duration::minutes(1))
}

#[derive(Debug, thiserror::Error)]
pub enum StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError {
    #[error(
        "Gradient did not send the access token for connection {id} on the Git host. This connection came from signing in at {oauth_app_host}. Its base URL is now empty. Sign in again to create a new connection, or set the base URL back to {oauth_app_host}."
    )]
    GitHostConnectionHasNoBaseUrl {
        id: GitHostConnectionId,
        oauth_app_host: String,
    },
    #[error(
        "Gradient did not send the access token for connection {id} to the Git host. This connection came from signing in at {oauth_app_host}. The base URL of this connection changed to {base_url_host}. Sign in again to create the connection anew or set the base URL back to {oauth_app_host}."
    )]
    ConnectionBaseUrlOriginDiffersFromOAuthAppOrigin {
        id: GitHostConnectionId,
        oauth_app_host: String,
        base_url_host: String,
    },
    #[error(
        "Gradient did not send the access token of connection {id} to the Git host. An administrator configured a Gitea application in the server configuration. This connection resulted from a sign-in through that application. Gradient cannot read the URL of that application, so Gradient does not confirm that the base URL of this connection is that host. Ask an administrator to correct the URL of the Gitea application in the server configuration."
    )]
    GiteaAppBaseUrlCannotBeParsed { id: GitHostConnectionId },
}

fn refusal_for_refresh_token_with_missing_or_different_gitea_oauth_base_url_origin(
    connection: &MGitHostConnection,
    app: &GiteaOAuthConfig,
) -> Option<StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError> {
    connection.encrypted_refresh_token.as_ref()?;
    refusal_for_missing_or_different_gitea_oauth_base_url_origin(connection, app)
}

fn refusal_for_missing_or_different_gitea_oauth_base_url_origin(
    connection: &MGitHostConnection,
    app: &GiteaOAuthConfig,
) -> Option<StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError> {
    let id = connection.id;
    let Some(oauth_app_host) = url_origin_without_credentials(&app.instance_base_url) else {
        return Some(
            StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError::GiteaAppBaseUrlCannotBeParsed { id },
        );
    };
    match connection.base_url.as_deref() {
        None => Some(
            StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError::GitHostConnectionHasNoBaseUrl {
                id,
                oauth_app_host,
            },
        ),
        Some(base_url) if urls_differ_in_scheme_host_or_port(base_url, &app.instance_base_url) => Some(
            StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError::ConnectionBaseUrlOriginDiffersFromOAuthAppOrigin {
                id,
                oauth_app_host,
                base_url_host: url_origin_without_credentials(base_url).unwrap_or_default(),
            },
        ),
        Some(_) => None,
    }
}

fn decrypt_access_token(
    crypt_file: &str,
    connection: &MGitHostConnection,
) -> Result<Option<SecretString>> {
    connection
        .access_token
        .as_deref()
        .map(|encrypted| decrypt_secret_with_file(crypt_file, encrypted))
        .transpose()
        .context("Decrypt the access token of the Git host connection")
}

pub async fn refresh_and_get_usable_decrypted_access_token_for_task_repository(
    db: &impl TransactionTrait,
    config: &RuntimeConfig,
    http: &gradient_util::http::HttpClientForUserSuppliedUrls,
    allowed: &GitHostConnectionRepositoryAccessProof,
) -> Result<Option<SecretString>> {
    refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
        db,
        config,
        http,
        allowed.connection(),
    )
    .await
}

/// This function is for connection owners with manage permission only. The connection test is one example. The entry point for code for a task is refresh_and_get_usable_decrypted_access_token_for_task_repository. The first step in that function is proof of the task's permission to use this connection for its own repository.
pub async fn refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
    db: &impl TransactionTrait,
    config: &RuntimeConfig,
    http: &gradient_util::http::HttpClientForUserSuppliedUrls,
    connection: &MGitHostConnection,
) -> Result<Option<SecretString>> {
    if let Some(refusal) = config.gitea_app.as_ref().and_then(|app| {
        refusal_for_refresh_token_with_missing_or_different_gitea_oauth_base_url_origin(
            connection, app,
        )
    }) {
        return Err(refusal.into());
    }
    let crypt_file = config.secrets.crypt_file.as_str();
    let Some(app) = config.gitea_app.as_ref().filter(|_| {
        git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute(
            connection,
            gradient_types::now(),
        )
    }) else {
        return decrypt_access_token(crypt_file, connection);
    };

    let txn = db.begin().await?;
    let Some(locked) = EGitHostConnection::find_by_id(connection.id)
        .lock(LockType::NoKeyUpdate)
        .one(&txn)
        .await
        .context("Lock the connection to the Git host for token refresh.")?
    else {
        return Ok(None);
    };
    if let Some(refusal) =
        refusal_for_refresh_token_with_missing_or_different_gitea_oauth_base_url_origin(
            &locked, app,
        )
    {
        return Err(refusal.into());
    }
    let git_host_target_changed = locked.git_host_type != connection.git_host_type
        || match (locked.base_url.as_deref(), connection.base_url.as_deref()) {
            (Some(locked), Some(connection)) => {
                urls_differ_in_scheme_host_or_port(locked, connection)
            }
            (locked, connection) => locked.is_some() != connection.is_some(),
        };
    if git_host_target_changed {
        return refusal_for_missing_or_different_gitea_oauth_base_url_origin(&locked, app)
            .map_or(Ok(None), |refusal| Err(refusal.into()));
    }
    let now = gradient_types::now();
    let refresh_token = match locked.encrypted_refresh_token.as_deref() {
        Some(encrypted)
            if git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute(
                &locked, now,
            ) =>
        {
            decrypt_secret_with_file(crypt_file, encrypted)?
        }
        _ => {
            txn.commit().await?;
            return decrypt_access_token(crypt_file, &locked);
        }
    };

    let tokens = get_new_tokens_from_git_host_with_refresh_token(http, app, refresh_token.expose())
        .await
        .with_context(|| {
            format!(
                "Refresh the OAuth2 access token for connection {id} to a Git host",
                id = locked.id
            )
        })?;
    let mut refreshed = locked;
    refreshed.access_token = Some(encrypt_secret_with_file(crypt_file, &tokens.access_token)?);
    if let Some(rotated) = tokens.refresh_token.as_deref() {
        refreshed.encrypted_refresh_token = Some(encrypt_secret_with_file(crypt_file, rotated)?);
    }
    refreshed.access_token_expires_at =
        access_token_expires_at_from_lifetime_seconds(now, tokens.expires_in);
    gradient_db::git_host_connections::write_all_columns_of_git_host_connection(&txn, refreshed)
        .await
        .context("Save the refreshed token for the Git host connection.")?;
    txn.commit().await?;
    Ok(Some(SecretString::from(tokens.access_token)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(refresh: Option<&str>, expires_at: Option<NaiveDateTime>) -> MGitHostConnection {
        MGitHostConnection {
            git_host_type: GitHostType::Forgejo,
            base_url: Some("https://codeberg.org".into()),
            access_token: Some("encrypted".into()),
            encrypted_refresh_token: refresh.map(str::to_owned),
            access_token_expires_at: expires_at,
            ..Default::default()
        }
    }

    #[test]
    fn only_expiring_tokens_with_a_refresh_token_need_a_refresh() {
        let now = gradient_types::now();
        let soon = Some(now + Duration::seconds(30));
        let later = Some(now + Duration::hours(1));
        let check = git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute;

        assert!(check(&connection(Some("r"), soon), now));
        assert!(check(
            &connection(Some("r"), Some(now - Duration::hours(1))),
            now
        ));
        assert!(!check(&connection(Some("r"), later), now));
        assert!(!check(&connection(None, soon), now));
        assert!(!check(&connection(Some("r"), None), now));
    }

    #[test]
    fn connection_refresh_token_base_url_same_host_but_different_port_or_http_is_not_same_origin() {
        let app = GiteaOAuthConfig {
            instance_base_url: "https://codeberg.org".into(),
            git_host_type: GitHostType::Forgejo,
            client_id: "client".into(),
            client_secret_file: "/run/secret".into(),
        };
        let with_base_url = |base_url: &str| MGitHostConnection {
            base_url: Some(base_url.into()),
            ..connection(Some("r"), None)
        };

        for refused in ["https://codeberg.org:8443", "http://codeberg.org"] {
            assert!(
                refusal_for_refresh_token_with_missing_or_different_gitea_oauth_base_url_origin(
                    &with_base_url(refused),
                    &app
                )
                .is_some(),
                "{refused}"
            );
        }
        for allowed in ["https://codeberg.org:443/", "https://CODEBERG.org/path"] {
            assert!(
                refusal_for_refresh_token_with_missing_or_different_gitea_oauth_base_url_origin(
                    &with_base_url(allowed),
                    &app
                )
                .is_none(),
                "{allowed}"
            );
        }
    }

    #[test]
    fn access_token_refusal_message_names_connection_and_hosts_without_credentials() {
        let app = GiteaOAuthConfig {
            instance_base_url: "https://codeberg.org".into(),
            git_host_type: GitHostType::Forgejo,
            client_id: "client".into(),
            client_secret_file: "/run/secret".into(),
        };
        let elsewhere = MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            base_url: Some("https://user:secret@gitea.example.com:3000/path?x=1".into()),
            ..connection(Some("r"), None)
        };
        let over_http = MGitHostConnection {
            base_url: Some("http://codeberg.org".into()),
            ..elsewhere.clone()
        };
        let without_base_url = MGitHostConnection {
            base_url: None,
            ..elsewhere.clone()
        };
        let message_for = |connection: &MGitHostConnection| {
            refusal_for_refresh_token_with_missing_or_different_gitea_oauth_base_url_origin(
                connection, &app,
            )
            .expect("refusal")
            .to_string()
        };

        let message = message_for(&elsewhere);
        assert!(message.contains(&elsewhere.id.to_string()), "{message}");
        assert!(message.contains("https://codeberg.org"), "{message}");
        assert!(
            message.contains("https://gitea.example.com:3000"),
            "{message}"
        );
        for leaked in ["user", "secret", "/path", "x=1"] {
            assert!(!message.contains(leaked), "{leaked}: {message}");
        }
        let message = message_for(&over_http);
        assert!(message.contains("https://codeberg.org"), "{message}");
        assert!(message.contains("http://codeberg.org"), "{message}");
        let message = message_for(&without_base_url);
        assert!(message.contains(&elsewhere.id.to_string()), "{message}");
        assert!(message.contains("https://codeberg.org"), "{message}");
    }

    #[test]
    fn refusal_for_unparsable_gitea_application_url_names_server_configuration_without_empty_host()
    {
        let app = GiteaOAuthConfig {
            instance_base_url: "not a url".into(),
            git_host_type: GitHostType::Forgejo,
            client_id: "client".into(),
            client_secret_file: "/run/secret".into(),
        };
        let connection = MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            ..connection(Some("r"), None)
        };

        let message =
            refusal_for_refresh_token_with_missing_or_different_gitea_oauth_base_url_origin(
                &connection,
                &app,
            )
            .expect("refusal")
            .to_string();
        assert!(message.contains(&connection.id.to_string()), "{message}");
        assert!(message.contains("server configuration"), "{message}");
        for empty_host in [" at .", " to .", " to  ", " at  "] {
            assert!(!message.contains(empty_host), "{empty_host}: {message}");
        }
    }

    fn clone_runtime_config_with_temp_crypt_file_and_forgejo_oauth_app(
        config: &RuntimeConfig,
        instance_base_url: String,
    ) -> (
        RuntimeConfig,
        tempfile::NamedTempFile,
        tempfile::NamedTempFile,
    ) {
        let crypt_file = tempfile::NamedTempFile::new().expect("crypt file");
        std::fs::write(crypt_file.path(), "test-secret-key-32-bytes-padding!")
            .expect("write crypt file");
        let client_secret_file = tempfile::NamedTempFile::new().expect("client secret file");
        std::fs::write(client_secret_file.path(), "s3cret\n").expect("write client secret");
        let mut config = config.clone();
        config.secrets.crypt_file = crypt_file.path().to_string_lossy().into_owned();
        config.gitea_app = Some(GiteaOAuthConfig {
            instance_base_url,
            git_host_type: GitHostType::Forgejo,
            client_id: "client".into(),
            client_secret_file: client_secret_file.path().to_string_lossy().into_owned(),
        });
        (config, crypt_file, client_secret_file)
    }

    #[tokio::test]
    async fn token_refresh_for_connection_with_base_url_on_other_host_than_oauth_app_is_refused_without_requests_and_row_stays_unchanged()
     {
        use crate::actions::tests::fixtures::make_ci_context_from_worker_and_web_databases;
        use gradient_db::rows;
        use gradient_entity::{git_host_connection, user};
        use gradient_fixtures::database::TestDatabase;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let oauth_app_server = MockServer::start().await;
        let other_host_server = MockServer::start().await;
        for server in [&oauth_app_server, &other_host_server] {
            Mock::given(any())
                .respond_with(ResponseTemplate::new(200))
                .expect(0)
                .mount(server)
                .await;
        }

        let database = TestDatabase::create().await;
        let db = database.connection();
        let ctx = make_ci_context_from_worker_and_web_databases(db.clone(), db.clone());
        let (config, _crypt_file, _client_secret_file) =
            clone_runtime_config_with_temp_crypt_file_and_forgejo_oauth_app(
                &ctx.db.config,
                oauth_app_server.uri(),
            );
        let crypt = config.secrets.crypt_file.as_str();

        rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
            .await
            .expect("user");
        let stored = rows::insert::<git_host_connection::Entity, _>(
            &db,
            git_host_connection::Model {
                id: GitHostConnectionId::now_v7(),
                name: "forgejo".into(),
                display_name: "Forgejo".into(),
                git_host_type: GitHostType::Forgejo,
                base_url: Some(other_host_server.uri().replace("127.0.0.1", "localhost")),
                access_token: Some(encrypt_secret_with_file(crypt, "old-access").expect("encrypt")),
                encrypted_refresh_token: Some(
                    encrypt_secret_with_file(crypt, "old-refresh").expect("encrypt"),
                ),
                access_token_expires_at: Some(gradient_types::now() - Duration::hours(1)),
                created_by: gradient_fixtures::ids::user_id(),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("connection");

        let refused = refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
            &db,
            &config,
            &ctx.db.user_supplied_url_http_client,
            &stored,
        )
        .await;

        let refusal = refused.expect_err("refusal");
        assert!(
            matches!(
                refusal.downcast_ref::<StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError>(),
                Some(StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError::ConnectionBaseUrlOriginDiffersFromOAuthAppOrigin { id, .. }) if *id == stored.id
            ),
            "{refusal}"
        );
        let saved = EGitHostConnection::find_by_id(stored.id)
            .one(&db)
            .await
            .expect("load")
            .expect("connection row");
        assert!(saved == stored);
        database.finish().await;
    }

    #[tokio::test]
    async fn expiring_access_token_of_connection_owner_is_refreshed_and_stored() {
        use crate::actions::tests::fixtures::make_ci_context_from_worker_and_web_databases;
        use gradient_db::rows;
        use gradient_entity::{git_host_connection, user};
        use gradient_fixtures::database::TestDatabase;
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "expires_in": 3600,
                "token_type": "bearer",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let database = TestDatabase::create().await;
        let db = database.connection();
        let ctx = make_ci_context_from_worker_and_web_databases(db.clone(), db.clone());
        let (config, _crypt_file, _client_secret_file) =
            clone_runtime_config_with_temp_crypt_file_and_forgejo_oauth_app(
                &ctx.db.config,
                server.uri(),
            );
        let crypt = config.secrets.crypt_file.as_str();

        rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
            .await
            .expect("user");
        let stored = rows::insert::<git_host_connection::Entity, _>(
            &db,
            git_host_connection::Model {
                id: GitHostConnectionId::now_v7(),
                name: "forgejo".into(),
                display_name: "Forgejo".into(),
                git_host_type: GitHostType::Forgejo,
                base_url: Some(server.uri()),
                access_token: Some(encrypt_secret_with_file(crypt, "old-access").expect("encrypt")),
                encrypted_refresh_token: Some(
                    encrypt_secret_with_file(crypt, "old-refresh").expect("encrypt"),
                ),
                access_token_expires_at: Some(gradient_types::now() - Duration::hours(1)),
                created_by: gradient_fixtures::ids::user_id(),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("connection");

        let token = refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
            &db,
            &config,
            &ctx.db.user_supplied_url_http_client,
            &stored,
        )
        .await
        .expect("refresh")
        .expect("access token");
        assert_eq!(token.expose(), "new-access");

        let saved = EGitHostConnection::find_by_id(stored.id)
            .one(&db)
            .await
            .expect("load")
            .expect("connection row");
        let decrypt = |encrypted: Option<String>| {
            decrypt_secret_with_file(crypt, &encrypted.expect("encrypted value")).expect("decrypt")
        };
        assert_eq!(decrypt(saved.access_token).expose(), "new-access");
        assert_eq!(
            decrypt(saved.encrypted_refresh_token).expose(),
            "new-refresh"
        );
        assert!(
            saved.access_token_expires_at.is_some_and(
                |expires_at| expires_at > gradient_types::now() + Duration::minutes(50)
            )
        );
        database.finish().await;
    }

    #[tokio::test]
    async fn repository_grant_insert_during_slow_token_refresh_does_not_wait_for_connection_row_lock()
     {
        use crate::actions::tests::fixtures::make_ci_context_from_worker_and_web_databases;
        use gradient_db::rows;
        use gradient_entity::{
            git_host_connection, git_host_connection_repository_grant, project, team, user,
        };
        use gradient_fixtures::database::TestDatabase;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "access_token": "new-access",
                        "refresh_token": "new-refresh",
                        "expires_in": 3600,
                        "token_type": "bearer",
                    }))
                    .set_delay(std::time::Duration::from_secs(3)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let database = TestDatabase::create().await;
        let db = database.connection();
        let ctx = make_ci_context_from_worker_and_web_databases(db.clone(), db.clone());
        let (config, _crypt_file, _client_secret_file) =
            clone_runtime_config_with_temp_crypt_file_and_forgejo_oauth_app(
                &ctx.db.config,
                server.uri(),
            );
        let crypt = config.secrets.crypt_file.as_str();

        rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
            .await
            .expect("user");
        rows::insert::<team::Entity, _>(&db, gradient_fixtures::rows::team())
            .await
            .expect("team");
        let project = rows::insert::<project::Entity, _>(&db, gradient_fixtures::rows::project())
            .await
            .expect("project");
        let stored = rows::insert::<git_host_connection::Entity, _>(
            &db,
            git_host_connection::Model {
                id: GitHostConnectionId::now_v7(),
                name: "forgejo".into(),
                display_name: "Forgejo".into(),
                git_host_type: GitHostType::Forgejo,
                base_url: Some(server.uri()),
                access_token: Some(encrypt_secret_with_file(crypt, "old-access").expect("encrypt")),
                encrypted_refresh_token: Some(
                    encrypt_secret_with_file(crypt, "old-refresh").expect("encrypt"),
                ),
                access_token_expires_at: Some(gradient_types::now() - Duration::hours(1)),
                created_by: gradient_fixtures::ids::user_id(),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("connection");

        let (token, repository_grant_row_insert_with_timeout_result) = tokio::join!(
            refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
                &db,
                &config,
                &ctx.db.user_supplied_url_http_client,
                &stored,
            ),
            async {
                while server
                    .received_requests()
                    .await
                    .is_none_or(|requests| requests.is_empty())
                {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    rows::insert::<git_host_connection_repository_grant::Entity, _>(
                        &db,
                        git_host_connection_repository_grant::Model {
                            id: GitHostConnectionRepositoryGrantId::now_v7(),
                            git_host_connection: stored.id,
                            project: project.id,
                            repository_owner: "owner".into(),
                            created_at: gradient_fixtures::test_date(),
                            ..Default::default()
                        },
                    ),
                )
                .await
            }
        );

        repository_grant_row_insert_with_timeout_result
            .expect("A grant insert completed while refresh held the row lock.")
            .expect("grant");
        assert_eq!(
            token.expect("refresh").expect("access token").expose(),
            "new-access"
        );
        database.finish().await;
    }

    #[tokio::test]
    async fn refresh_decrypted_access_token_with_old_row_snapshot_after_base_url_change_is_refused_without_http_request_and_row_is_unchanged()
     {
        use crate::actions::tests::fixtures::make_ci_context_from_worker_and_web_databases;
        use gradient_db::rows;
        use gradient_entity::{git_host_connection, user};
        use gradient_fixtures::database::TestDatabase;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let oauth_app_server = MockServer::start().await;
        let new_git_host_server = MockServer::start().await;
        for server in [&oauth_app_server, &new_git_host_server] {
            Mock::given(any())
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "expires_in": 3600,
                    "token_type": "bearer",
                })))
                .mount(server)
                .await;
        }

        let database = TestDatabase::create().await;
        let db = database.connection();
        let ctx = make_ci_context_from_worker_and_web_databases(db.clone(), db.clone());
        let (config, _crypt_file, _client_secret_file) =
            clone_runtime_config_with_temp_crypt_file_and_forgejo_oauth_app(
                &ctx.db.config,
                oauth_app_server.uri(),
            );
        let crypt = config.secrets.crypt_file.as_str();

        rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
            .await
            .expect("user");
        let stored = rows::insert::<git_host_connection::Entity, _>(
            &db,
            git_host_connection::Model {
                id: GitHostConnectionId::now_v7(),
                name: "forgejo".into(),
                display_name: "Forgejo".into(),
                git_host_type: GitHostType::Forgejo,
                base_url: Some(oauth_app_server.uri()),
                access_token: Some(encrypt_secret_with_file(crypt, "old-access").expect("encrypt")),
                encrypted_refresh_token: Some(
                    encrypt_secret_with_file(crypt, "old-refresh").expect("encrypt"),
                ),
                access_token_expires_at: Some(gradient_types::now() - Duration::hours(1)),
                created_by: gradient_fixtures::ids::user_id(),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("connection");
        let git_host_connection_row_after_other_writer_moved_base_url_to_other_host_and_kept_refresh_token =
            gradient_db::git_host_connections::write_all_columns_of_git_host_connection(
                &db,
                git_host_connection::Model {
                    base_url: Some(new_git_host_server.uri()),
                    ..stored.clone()
                },
            )
            .await
            .expect("move base URL");

        let refused = refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
            &db,
            &config,
            &ctx.db.user_supplied_url_http_client,
            &stored,
        )
        .await;

        let refusal = refused.expect_err("refusal");
        assert!(
            matches!(
                refusal.downcast_ref::<StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError>(),
                Some(StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError::ConnectionBaseUrlOriginDiffersFromOAuthAppOrigin { id, .. }) if *id == stored.id
            ),
            "{refusal}"
        );
        for server in [&oauth_app_server, &new_git_host_server] {
            assert!(
                server
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .is_empty()
            );
        }
        let saved = EGitHostConnection::find_by_id(stored.id)
            .one(&db)
            .await
            .expect("load")
            .expect("connection row");
        assert!(
            saved
                == git_host_connection_row_after_other_writer_moved_base_url_to_other_host_and_kept_refresh_token
        );
        database.finish().await;
    }

    #[tokio::test]
    async fn refresh_with_old_snapshot_refuses_to_return_token_pasted_for_new_base_url_and_sends_no_http_request()
     {
        use crate::actions::tests::fixtures::make_ci_context_from_worker_and_web_databases;
        use gradient_db::rows;
        use gradient_entity::{git_host_connection, user};
        use gradient_fixtures::database::TestDatabase;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let oauth_app_server = MockServer::start().await;
        let new_git_host_server = MockServer::start().await;
        for server in [&oauth_app_server, &new_git_host_server] {
            Mock::given(any())
                .respond_with(ResponseTemplate::new(200))
                .mount(server)
                .await;
        }

        let database = TestDatabase::create().await;
        let db = database.connection();
        let ctx = make_ci_context_from_worker_and_web_databases(db.clone(), db.clone());
        let (config, _crypt_file, _client_secret_file) =
            clone_runtime_config_with_temp_crypt_file_and_forgejo_oauth_app(
                &ctx.db.config,
                oauth_app_server.uri(),
            );
        let crypt = config.secrets.crypt_file.as_str();

        rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
            .await
            .expect("user");
        let stored = rows::insert::<git_host_connection::Entity, _>(
            &db,
            git_host_connection::Model {
                id: GitHostConnectionId::now_v7(),
                name: "forgejo".into(),
                display_name: "Forgejo".into(),
                git_host_type: GitHostType::Forgejo,
                base_url: Some(oauth_app_server.uri()),
                access_token: Some(encrypt_secret_with_file(crypt, "old-access").expect("encrypt")),
                encrypted_refresh_token: Some(
                    encrypt_secret_with_file(crypt, "old-refresh").expect("encrypt"),
                ),
                access_token_expires_at: Some(gradient_types::now() - Duration::hours(1)),
                created_by: gradient_fixtures::ids::user_id(),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("connection");
        let git_host_connection_row_after_base_url_change_with_new_access_token_and_no_refresh_token_or_expiry =
            gradient_db::git_host_connections::write_all_columns_of_git_host_connection(
                &db,
                git_host_connection::Model {
                    base_url: Some(new_git_host_server.uri()),
                    access_token: Some(
                        encrypt_secret_with_file(crypt, "new-access").expect("encrypt"),
                    ),
                    encrypted_refresh_token: None,
                    access_token_expires_at: None,
                    ..stored.clone()
                },
            )
            .await
            .expect("move base URL");

        let refused = refresh_and_get_usable_decrypted_access_token_for_git_host_connection_owner(
            &db,
            &config,
            &ctx.db.user_supplied_url_http_client,
            &stored,
        )
        .await;

        let refusal = refused.expect_err("refusal");
        assert!(
            matches!(
                refusal.downcast_ref::<StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError>(),
                Some(StoredAccessTokenNotSentForMissingOrDifferentBaseUrlOriginError::ConnectionBaseUrlOriginDiffersFromOAuthAppOrigin { id, .. }) if *id == stored.id
            ),
            "{refusal}"
        );
        for server in [&oauth_app_server, &new_git_host_server] {
            assert!(
                server
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .is_empty()
            );
        }
        let saved = EGitHostConnection::find_by_id(stored.id)
            .one(&db)
            .await
            .expect("load")
            .expect("connection row");
        assert!(
            saved
                == git_host_connection_row_after_base_url_change_with_new_access_token_and_no_refresh_token_or_expiry
        );
        database.finish().await;
    }

    #[test]
    fn expiry_adds_the_lifetime_to_now() {
        let now = gradient_types::now();
        assert_eq!(
            access_token_expires_at_from_lifetime_seconds(now, Some(3600)),
            Some(now + Duration::hours(1))
        );
        assert_eq!(
            access_token_expires_at_from_lifetime_seconds(now, None),
            None
        );
    }
}
