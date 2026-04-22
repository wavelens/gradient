/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Routes under `/api/v1/admin/*`. All handlers must be superuser-gated via
//! `require_superuser`.

pub mod github_app;
pub mod workers;

use axum::Router;
use axum::routing::{get, post};
use core::types::ServerState;
use std::sync::Arc;

/// Returns the sub-router that is nested under `/admin` by `create_router`.
pub fn admin_router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/workers", get(workers::get_workers))
        .route("/github-app/manifest", post(github_app::request_manifest))
        .route("/github-app/callback", get(github_app::callback))
        .route("/github-app/credentials", get(github_app::credentials))
}
