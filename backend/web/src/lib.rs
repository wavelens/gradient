/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod auth;
mod endpoint;
pub mod requests;

use axum::routing::{get, post};
use axum::{middleware, Router};

use core::types::ServerState;
use std::sync::Arc;

pub async fn serve_web(state: Arc<ServerState>) -> std::io::Result<()> {
    let server_url = format!("{}:{}", state.cli.ip.clone(), state.cli.port.clone());
    let app = Router::new()
        .route(
            "/api/organization",
            get(endpoint::get_organizations).post(endpoint::post_organizations),
        )
        .route(
            "/api/organization/:organization",
            get(endpoint::get_organization).post(endpoint::post_organization),
        )
        .route(
            "/api/organization/:organization/ssh",
            get(endpoint::get_organization_ssh).post(endpoint::post_organization_ssh),
        )
        .route(
            "/api/project/:project",
            get(endpoint::get_project).post(endpoint::post_project),
        )
        .route(
            "/api/project/:project/check-repository",
            post(endpoint::post_project_check_repository),
        )
        .route(
            "/api/build/:build",
            get(endpoint::get_build).post(endpoint::post_build),
        )
        .route(
            "/api/user/settings/:user",
            get(endpoint::get_user).post(endpoint::post_user),
        )
        .route("/api/user/api", post(endpoint::post_api_key))
        .route(
            "/api/server",
            get(endpoint::get_servers).post(endpoint::post_servers),
        )
        .route(
            "/api/server/:server/check",
            post(endpoint::post_server_check),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::authorize,
        ))
        .route("/api/user/login", post(endpoint::post_login))
        .route("/api/user/logout", post(endpoint::post_logout))
        .route("/api/user/register", post(endpoint::post_register))
        .route("/api/health", get(endpoint::get_health))
        .fallback(endpoint::handle_404)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await
}
