/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod draining;
pub mod github_app;
pub mod maintenance;
pub mod state;
pub mod tasks;
pub mod workers;

use crate::error::{WebResult, require_superuser};
use axum::extract::Request;
use axum::middleware::{Next, from_fn};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Router};
use gradient_core::ServerState;
use gradient_types::MUser;
use std::sync::Arc;

async fn superuser_only(
    Extension(user): Extension<MUser>,
    req: Request,
    next: Next,
) -> WebResult<Response> {
    require_superuser(&user)?;

    Ok(next.run(req).await)
}

pub fn admin_router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/state", get(state::export_state))
        .route("/github-app/manifest", post(github_app::request_manifest))
        .route("/github-app/credentials", get(github_app::credentials))
        .route("/maintenance/deep-gc", post(maintenance::start_deep_gc))
        .route("/draining", post(draining::set_draining))
        .route("/tasks", get(tasks::list_tasks))
        .route("/tasks/{task_id}", get(tasks::get_task))
        .nest("/webhooks", super::webhooks::router())
        .route_layer(from_fn(superuser_only))
        .route("/workers", get(workers::get_workers))
}
