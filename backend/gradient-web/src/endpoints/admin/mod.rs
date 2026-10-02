/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! All handlers must be superuser-gated via `require_superuser`.

pub mod base_workers;
pub mod draining;
pub mod github_app;
pub mod maintenance;
pub mod state;
pub mod tasks;
pub mod workers;

use axum::Router;
use axum::routing::{delete, get, post};
use gradient_core::ServerState;
use std::sync::Arc;

pub fn admin_router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/workers", get(workers::get_workers))
        .route("/base-workers", get(base_workers::get_base_workers))
        .route(
            "/base-workers/{worker_id}",
            delete(base_workers::delete_base_worker),
        )
        .route("/state", get(state::export_state))
        .route("/github-app/manifest", post(github_app::request_manifest))
        .route("/github-app/credentials", get(github_app::credentials))
        .route("/maintenance/deep-gc", post(maintenance::start_deep_gc))
        .route("/draining", post(draining::set_draining))
        .route("/tasks", get(tasks::list_tasks))
        .route("/tasks/{task_id}", get(tasks::get_task))
        .nest("/webhooks", super::webhooks::router())
}
