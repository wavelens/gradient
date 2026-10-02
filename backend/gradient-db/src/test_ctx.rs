/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use clap::Parser as _;
use gradient_storage::{FileLogStorage, NarStore, StorageCtx};
use gradient_types::{Cli, RuntimeConfig};
use gradient_util::shutdown::Shutdown;
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

use crate::{DbContext, ProbeRequests, WebDb, WorkerDb};

/// Spawned tasks are holding `DbContext` clones that outlive a plain drop.
/// `WorkerDb::into_transaction_log` is requiring the builder's handle to be the last one.
pub(crate) async fn settle(ctx: DbContext) {
    ctx.shutdown
        .cancel_and_drain(std::time::Duration::from_secs(5))
        .await;
    drop(ctx);
}

pub(crate) fn inserted_phase_event() -> Vec<std::collections::BTreeMap<String, sea_orm::Value>> {
    vec![std::collections::BTreeMap::from([(
        "id".to_owned(),
        sea_orm::Value::from(uuid::Uuid::now_v7()),
    )])]
}

pub(crate) async fn ctx(db: DatabaseConnection) -> (DbContext, WorkerDb) {
    let (ctx, pool, _probes) = ctx_with_probes(db).await;
    (ctx, pool)
}

pub(crate) async fn ctx_with_probes(
    db: DatabaseConnection,
) -> (
    DbContext,
    WorkerDb,
    tokio::sync::mpsc::UnboundedReceiver<Vec<gradient_types::DerivationId>>,
) {
    let dir = std::env::temp_dir().join(format!("gradient-db-{}", uuid::Uuid::now_v7()));
    let probe_requests = ProbeRequests::channel();
    let probes = probe_requests
        .take_inbox()
        .expect("a fresh channel has one");
    let (ctx, pool) = ctx_at(db, &dir).await;
    (
        DbContext {
            probe_requests,
            ..ctx
        },
        pool,
        probes,
    )
}

pub(crate) async fn ctx_at(db: DatabaseConnection, dir: &std::path::Path) -> (DbContext, WorkerDb) {
    let path = dir.to_string_lossy().into_owned();
    let cli = Cli::try_parse_from([
        "gradient-server",
        "--secrets-crypt-file",
        "test-secret",
        "--secrets-jwt-file",
        "test-jwt",
        "--serve-url",
        "http://127.0.0.1:3000",
        "--base-dir",
        &path,
    ])
    .expect("test cli");

    let worker_db = WorkerDb::new(db);
    let ctx = DbContext {
        worker_db: worker_db.clone(),
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: Arc::new(RuntimeConfig::from_cli(&cli).expect("test config")),
        storage: StorageCtx {
            nar_storage: NarStore::local(&path).expect("test NarStore"),
            log_storage: Arc::new(FileLogStorage::new(dir).await.expect("test log storage")),
        },
        shutdown: Shutdown::new(),
        events: gradient_types::EventBus::new(16),
        delivery_wake: Default::default(),
        probe_requests: ProbeRequests::default(),
        startable_set: Default::default(),
    };

    (ctx, worker_db)
}
