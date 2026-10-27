/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod draining;
pub mod github_app;
pub mod maintenance;
mod refusal;
pub mod state;
pub mod tasks;
pub mod workers;

use crate::endpoints::webhooks::{self, WebhookMount};
use crate::error::{WebResult, require_superuser};
use axum::Extension;
use axum::extract::Request;
use axum::middleware::{Next, from_fn};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_types::MUser;
use std::sync::Arc;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

async fn superuser_only(
    Extension(user): Extension<MUser>,
    req: Request,
    next: Next,
) -> WebResult<Response> {
    require_superuser(&user)?;

    Ok(next.run(req).await)
}

pub fn admin_router() -> OpenApiRouter<Arc<ServerState>> {
    OpenApiRouter::new()
        .routes(routes!(state::export_state))
        .routes(routes!(github_app::request_manifest))
        .routes(routes!(github_app::credentials))
        .routes(routes!(maintenance::start_deep_gc))
        .routes(routes!(draining::set_draining))
        .routes(routes!(tasks::list_tasks))
        .routes(routes!(tasks::get_task))
        .nest("/webhooks", webhooks::router(WebhookMount::Instance))
        .route_layer(from_fn(superuser_only))
        .routes(routes!(crate::endpoints::workers::get_workers))
        .routes(routes!(crate::endpoints::workers::get_worker))
}
