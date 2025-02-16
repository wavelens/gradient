/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod auth;
mod endpoint;
pub mod requests;

use axum::body::Body;
use axum::routing::{get, post};
use axum::{middleware, Router};
use tower_http::trace::TraceLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use http::header::{AUTHORIZATION, ACCEPT, CONTENT_TYPE};
use http::{Request, Response, HeaderMap};
use bytes::Bytes;
use tower_http::classify::ServerErrorsFailureClass;
use std::time::Duration;
use tracing::Span;

use core::types::ServerState;
use std::sync::Arc;

pub async fn serve_web(state: Arc<ServerState>) -> std::io::Result<()> {
    let server_url = format!("{}:{}", state.cli.ip.clone(), state.cli.port.clone());

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::exact(state.cli.serve_url.clone().try_into().unwrap()))
        .allow_headers(vec![AUTHORIZATION, ACCEPT, CONTENT_TYPE])
        .allow_credentials(true);

    let trace = TraceLayer::new_for_http()
        .on_request(|request: &Request<Body>, _span: &Span| {
            tracing::debug!("started {} {}", request.method(), request.uri().path())
        })
        .on_response(|_response: &Response<Body>, latency: Duration, _span: &Span| {
            tracing::debug!("response generated in {:?}", latency)
        })
        .on_body_chunk(|chunk: &Bytes, _latency: Duration, _span: &Span| {
            tracing::debug!("sending {} bytes", chunk.len())
        })
        .on_eos(|_trailers: Option<&HeaderMap>, stream_duration: Duration, _span: &Span| {
            tracing::debug!("stream closed after {:?}", stream_duration)
        })
        .on_failure(|error: ServerErrorsFailureClass, _latency: Duration, _span: &Span| {
            tracing::debug!("request failed with {:?}", error)
        });

    let app = Router::new()
        .route(
            "/api/organization",
            get(endpoint::get_organizations).post(endpoint::post_organizations),
        )
        .route(
            "/api/organization/{organization}",
            get(endpoint::get_organization).post(endpoint::post_organization),
        )
        .route(
            "/api/organization/{organization}/ssh",
            get(endpoint::get_organization_ssh).post(endpoint::post_organization_ssh),
        )
        .route(
            "/api/organization/{organization}/projects",
            get(endpoint::get_organization_projects),
        )
        .route(
            "/api/project/{project}",
            get(endpoint::get_project).post(endpoint::post_project),
        )
        .route(
            "/api/project/{project}/check-repository",
            post(endpoint::post_project_check_repository),
        )
        .route(
            "/api/evaluation/{evaluation}",
            get(endpoint::get_evaluation).post(endpoint::post_evaluation),
        )
        .route(
            "/api/evaluation/{evaluation}/builds",
            get(endpoint::get_builds).post(endpoint::connect_evaluation),
        )
        .route(
            "/api/build/{build}",
            get(endpoint::get_build).post(endpoint::connect_build),
        )
        .route(
            "/api/user/settings/{user}",
            get(endpoint::get_user).post(endpoint::post_user),
        )
        .route("/api/user/api", post(endpoint::post_api_key))
        .route("/api/user/info", get(endpoint::get_user_info))
        .route(
            "/api/server",
            get(endpoint::get_servers).post(endpoint::post_servers),
        )
        .route(
            "/api/server/{server}/check",
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
        .layer(cors)
        .layer(trace)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await
}
