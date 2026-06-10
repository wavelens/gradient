/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::{ActionType, MProjectAction, ProjectId};
use serde_json::json;
use uuid::Uuid;

pub fn action_with(action_type: ActionType, events: Vec<&str>) -> MProjectAction {
    MProjectAction {
        id: gradient_types::ProjectActionId::now_v7(),
        project: ProjectId::new(Uuid::nil()),
        name: "t".into(),
        action_type: action_type.to_i16(),
        config: json!({}),
        events: json!(events),
        active: true,
        last_fired_at: None,
        created_by: gradient_types::UserId::new(Uuid::nil()),
        created_at: gradient_types::now(),
        updated_at: gradient_types::now(),
    }
}

pub fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

pub fn make_ctx() -> crate::ci::CiContext {
    use crate::ci::CiContext;
    use crate::db::{DbContext, NoReactor, WebDb, WorkerDb};
    use crate::storage::{EmailSender, LogStorage, NarStore, StorageCtx};
    use gradient_types::RuntimeConfig;
    use futures::future::BoxFuture;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[derive(Debug)]
    struct NoopLog;
    impl LogStorage for NoopLog {
        fn append<'a>(
            &'a self,
            _: gradient_entity::ids::BuildId,
            _: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn read<'a>(
            &'a self,
            _: gradient_entity::ids::BuildId,
        ) -> BoxFuture<'a, anyhow::Result<String>> {
            Box::pin(async { Ok(String::new()) })
        }
        fn delete<'a>(
            &'a self,
            _: gradient_entity::ids::BuildId,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn list_logs<'a>(
            &'a self,
        ) -> BoxFuture<'a, anyhow::Result<Vec<gradient_entity::ids::BuildId>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn write_chunk<'a>(
            &'a self,
            _: gradient_entity::ids::BuildId,
            _: u32,
            _: &'a [u8],
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn read_chunk<'a>(
            &'a self,
            _: gradient_entity::ids::BuildId,
            _: u32,
        ) -> BoxFuture<'a, anyhow::Result<Vec<u8>>> {
            Box::pin(async { anyhow::bail!("no chunk") })
        }
        fn delete_chunks<'a>(
            &'a self,
            _: gradient_entity::ids::BuildId,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Debug)]
    struct NoopEmail;
    #[async_trait::async_trait]
    impl EmailSender for NoopEmail {
        fn is_enabled(&self) -> bool {
            false
        }
        async fn send_verification_email(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_password_reset_email(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_action_mail(
            &self,
            _: &[String],
            _: &str,
            _: &str,
        ) -> anyhow::Result<crate::storage::email::MailDeliveryResult> {
            Ok(crate::storage::email::MailDeliveryResult {
                status_code: 0,
                server_response: String::new(),
            })
        }
    }

    let cli = gradient_types::Cli {
        logging: gradient_types::LoggingArgs::default(),
        server: gradient_types::ServerArgs::default(),
        database: gradient_types::DatabaseArgs::default(),
        eval: gradient_types::EvalArgs::default(),
        storage: gradient_types::StorageArgs {
            base_path: "/tmp/gradient-test".into(),
            ..Default::default()
        },
        secrets: gradient_types::SecretsArgs {
            crypt_secret_file: "test-secret".into(),
            jwt_secret_file: "test-jwt".into(),
        },
        limits: gradient_types::LimitsArgs::default(),
        registration: gradient_types::RegistrationArgs::default(),
        proto: gradient_types::ProtoArgs::default(),
        oidc: gradient_types::OidcArgs::default(),
        email: gradient_types::EmailArgs::default(),
        s3: gradient_types::S3Args::default(),
        github_app: gradient_types::GitHubAppArgs::default(),
        metrics: gradient_types::MetricsArgs::default(),
        network: gradient_types::NetworkArgs::default(),
    };
    let config = std::sync::Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
    let db = DbContext {
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        storage: StorageCtx {
            nar_storage,
            log_storage: std::sync::Arc::new(NoopLog),
            email: std::sync::Arc::new(NoopEmail) as std::sync::Arc<dyn EmailSender>,
        },
        shutdown: crate::shutdown::Shutdown::new(),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(NoReactor),
    };
    CiContext {
        db,
        http: crate::http::build_client().expect("http client"),
        forge: crate::forge::ForgeRegistry::with_builtin(),
    }
}
