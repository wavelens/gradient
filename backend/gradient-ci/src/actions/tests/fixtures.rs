/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::{ActionType, MTaskAction, TaskId};
use serde_json::json;
use uuid::Uuid;

pub fn action_with(action_type: ActionType, events: Vec<&str>) -> MTaskAction {
    MTaskAction {
        id: gradient_types::TaskActionId::now_v7(),
        task: TaskId::new(Uuid::nil()),
        name: "t".into(),
        action_type,
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

pub fn make_ctx() -> crate::CiContext {
    make_ctx_with(sea_orm::MockDatabase::new(
        sea_orm::DatabaseBackend::Postgres,
    ))
}

pub fn make_ctx_with(worker: sea_orm::MockDatabase) -> crate::CiContext {
    use crate::CiContext;
    use futures::future::BoxFuture;
    use gradient_db::{DbContext, WebDb, WorkerDb};
    use gradient_notify::EmailSender;
    use gradient_storage::{LogStorage, NarStore, StorageCtx};
    use gradient_types::RuntimeConfig;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[derive(Debug)]
    struct NoopLog;
    impl LogStorage for NoopLog {
        fn append<'a>(
            &'a self,
            _: gradient_entity::ids::BuildAttemptId,
            _: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn read<'a>(
            &'a self,
            _: gradient_entity::ids::BuildAttemptId,
        ) -> BoxFuture<'a, anyhow::Result<String>> {
            Box::pin(async { Ok(String::new()) })
        }
        fn delete<'a>(
            &'a self,
            _: gradient_entity::ids::BuildAttemptId,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn list_shard<'a>(
            &'a self,
            _shard: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<Vec<gradient_entity::ids::BuildAttemptId>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn write_chunk<'a>(
            &'a self,
            _: gradient_entity::ids::BuildAttemptId,
            _: u32,
            _: &'a [u8],
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn read_chunk<'a>(
            &'a self,
            _: gradient_entity::ids::BuildAttemptId,
            _: u32,
        ) -> BoxFuture<'a, anyhow::Result<Vec<u8>>> {
            Box::pin(async { anyhow::bail!("no chunk") })
        }
        fn delete_chunks<'a>(
            &'a self,
            _: gradient_entity::ids::BuildAttemptId,
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
        ) -> anyhow::Result<gradient_notify::MailDeliveryResult> {
            Ok(gradient_notify::MailDeliveryResult {
                status_code: 0,
                server_response: String::new(),
            })
        }
        async fn send_invitation_email(
            &self,
            _: &str,
            _: &str,
            _: &gradient_notify::InvitationMail<'_>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_subscription_mail(
            &self,
            _: &[String],
            _: &gradient_notify::SubscriptionMail<'_>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let cli = gradient_types::Cli {
        server: gradient_types::ServerArgs {
            base_dir: "/tmp/gradient-test".into(),
            ..Default::default()
        },
        secrets: gradient_types::SecretsArgs {
            crypt_file: "test-secret".into(),
            jwt_file: "test-jwt".into(),
        },
        ..Default::default()
    };
    let config = std::sync::Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.server.base_dir).expect("nar store");
    let db = DbContext {
        worker_db: WorkerDb::new(worker.into_connection()),
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        storage: StorageCtx {
            nar_storage,
            log_storage: std::sync::Arc::new(NoopLog),
        },
        shutdown: gradient_util::shutdown::Shutdown::new(),
        events: gradient_types::EventBus::default(),
        delivery_wake: Default::default(),
        probe_requests: Default::default(),
        held_evaluations: Default::default(),
        startable_set: Default::default(),
    };
    CiContext {
        db,
        http: gradient_util::http::build_client().expect("http client"),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        email: std::sync::Arc::new(NoopEmail) as std::sync::Arc<dyn EmailSender>,
    }
}
