/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ServerState;
use gradient_storage::{LogStorage, NarStore};
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::state::TestState;
use gradient_types::RuntimeConfig;
use sea_orm::DatabaseConnection;
use std::sync::Arc;

pub(crate) fn test_server_state(
    nar: NarStore,
    db: DatabaseConnection,
    configure: impl FnOnce(&mut RuntimeConfig) + 'static,
) -> Arc<ServerState> {
    test_server_state_with_log(nar, Arc::new(NoopLogStorage), db, configure)
}

pub(crate) fn test_server_state_with_log(
    nar: NarStore,
    log_storage: Arc<dyn LogStorage>,
    db: DatabaseConnection,
    configure: impl FnOnce(&mut RuntimeConfig) + 'static,
) -> Arc<ServerState> {
    TestState::new()
        .worker_db(db)
        .nar_storage(nar)
        .log_storage(log_storage)
        .config(configure)
        .build()
}

pub(crate) async fn store_exec_output(nar: &NarStore, job: uuid::Uuid, staging: &std::path::Path) {
    let staged = staging.join(format!("{job}.staged"));
    std::fs::write(&staged, b"weights").expect("the staged output is written");
    nar.adopt_exec_output(job, "model/w.bin", &staged)
        .await
        .expect("the output is stored");
}
