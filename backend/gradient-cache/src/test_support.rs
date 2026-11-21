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

pub(crate) async fn build_test_server_state_with_running_graph_writer(
    nar: NarStore,
    db: DatabaseConnection,
    configure: impl FnOnce(&mut RuntimeConfig) + 'static,
) -> Arc<ServerState> {
    let graph = gradient_graph::Graph::new();
    let state = TestState::new()
        .worker_db(db.clone())
        .cache_db(db.clone())
        .web_db(db)
        .nar_storage(nar)
        .log_storage(Arc::new(NoopLogStorage))
        .graph(Arc::clone(&graph))
        .config(configure)
        .build();
    graph
        .spawn(state.db(), None, None)
        .await
        .expect("the graph writer starts");
    state
}

pub(crate) async fn store_exec_output(nar: &NarStore, job: uuid::Uuid, staging: &std::path::Path) {
    let staged = staging.join(format!("{job}.staged"));
    std::fs::write(&staged, b"weights").expect("the staged output is written");
    nar.adopt_exec_output(job, "model/w.bin", &staged)
        .await
        .expect("the output is stored");
}
