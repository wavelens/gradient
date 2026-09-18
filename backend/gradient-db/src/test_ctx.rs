/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A [`DbContext`] over a scripted `MockDatabase`, for the `db` functions that
//! take one. Mirrors `gradient_graph::test_ctx`; every context gets its own
//! storage directory so parallel tests never share one.

use std::sync::Arc;

use clap::Parser as _;
use gradient_storage::{FileLogStorage, NarStore, StorageCtx};
use gradient_types::{Cli, RuntimeConfig};
use gradient_util::shutdown::Shutdown;
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

use crate::{DbContext, WebDb, WorkerDb};

/// Drain the detached work a context spawned, then drop it, so the `WorkerDb` the
/// builder returned is the last handle on the pool - which is what
/// `WorkerDb::into_transaction_log` requires. `drop(ctx)` alone is not enough for
/// any path that still spawns onto `ctx.shutdown`: each spawned task holds a
/// `DbContext` clone that outlives the drop.
pub(crate) async fn settle(ctx: DbContext) {
    ctx.shutdown
        .cancel_and_drain(std::time::Duration::from_secs(5))
        .await;
    drop(ctx);
}

/// A context over `db`, plus the pool handle its transaction log is read from.
/// Drop the context before draining that log: `WorkerDb::into_transaction_log`
/// requires the handle it is called on to be the last one alive.
pub(crate) async fn ctx(db: DatabaseConnection) -> (DbContext, WorkerDb) {
    let dir = std::env::temp_dir().join(format!("gradient-db-{}", uuid::Uuid::now_v7()));
    ctx_at(db, &dir).await
}

/// [`ctx`] over a caller-owned directory, for a test that must place a NAR object
/// where the code under test looks for it.
pub(crate) async fn ctx_at(db: DatabaseConnection, dir: &std::path::Path) -> (DbContext, WorkerDb) {
    let path = dir.to_string_lossy().into_owned();
    let cli = Cli::try_parse_from([
        "gradient-server",
        "--crypt-secret-file",
        "test-secret",
        "--jwt-secret-file",
        "test-jwt",
        "--serve-url",
        "http://127.0.0.1:3000",
        "--base-path",
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
        board_events: tokio::sync::broadcast::channel(16).0,
        outbox_wake: Default::default(),
    };

    (ctx, worker_db)
}
