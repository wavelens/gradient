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
use gradient_core::types::ServerState;
use std::sync::Arc;

/// Returns the sub-router that is nested under `/admin` by `create_router`.
///
/// The GitHub App manifest *callback* is intentionally NOT mounted here — it
/// is registered as a public route by `create_router` because GitHub's
/// browser redirect from github.com cannot carry the operator's bearer
/// token. CSRF/identity is bound through the one-shot `state` token instead.
pub fn admin_router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/workers", get(workers::get_workers))
        .route("/github-app/manifest", post(github_app::request_manifest))
        .route("/github-app/credentials", get(github_app::credentials))
}
