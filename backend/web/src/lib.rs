/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod authorization;
mod endpoints;

use axum::body::Body;
use axum::routing::{get, post};
use axum::{middleware, Router};
use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, Request, Response};
use std::time::Duration;
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::Span;

use core::types::ServerState;
use endpoints::*;
use std::sync::Arc;

pub async fn serve_web(state: Arc<ServerState>) -> std::io::Result<()> {
    let server_url = format!("{}:{}", state.cli.ip.clone(), state.cli.port.clone());

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::exact(
            state.cli.serve_url.clone().try_into().unwrap(),
        ))
        .allow_headers(vec![AUTHORIZATION, ACCEPT, CONTENT_TYPE])
        .allow_credentials(true);

    let trace = TraceLayer::new_for_http()
        .on_request(|request: &Request<Body>, _span: &Span| {
            tracing::debug!("started {} {}", request.method(), request.uri().path())
        })
        .on_response(
            |_response: &Response<Body>, latency: Duration, _span: &Span| {
                tracing::debug!("response generated in {:?}", latency)
            },
        )
        .on_body_chunk(|chunk: &Bytes, _latency: Duration, _span: &Span| {
            tracing::debug!("sending {} bytes", chunk.len())
        })
        .on_eos(
            |_trailers: Option<&HeaderMap>, stream_duration: Duration, _span: &Span| {
                tracing::debug!("stream closed after {:?}", stream_duration)
            },
        )
        .on_failure(
            |error: ServerErrorsFailureClass, _latency: Duration, _span: &Span| {
                tracing::debug!("request failed with {:?}", error)
            },
        );

    let api = Router::new()
        .route("/orgs", get(orgs::get).post(orgs::post))
        .route(
            "/orgs/{organization}",
            get(orgs::get_organization).delete(orgs::delete_organization),
        )
        .route(
            "/orgs/{organization}/ssh",
            get(orgs::get_organization_ssh).post(orgs::post_organization_ssh),
        )
        .route(
            "/projects/{organization}",
            get(projects::get).post(projects::post),
        )
        .route(
            "/projects/{organization}/{project}",
            get(projects::get_project).delete(projects::delete_project),
        )
        .route(
            "/projects/{organization}/{project}/check-repository",
            post(projects::post_project_check_repository),
        )
        .route(
            "/projects/{organization}/{project}/evaluate",
            post(projects::post_project_evaluate),
        )
        .route(
            "/evals/{evaluation}",
            get(evals::get_evaluation).post(evals::post_evaluation),
        )
        .route(
            "/evals/{evaluation}/builds",
            get(evals::get_evaluation_builds).post(evals::connect_evaluation_builds),
        )
        .route(
            "/builds/{build}",
            get(builds::get_build).post(builds::connect_build),
        )
        .route("/user", get(user::get).delete(user::delete))
        .route(
            "/user/keys",
            get(user::get_keys)
                .post(user::post_keys)
                .delete(user::delete_keys),
        )
        .route(
            "/user/settings",
            get(user::get_settings).post(user::post_settings),
        )
        .route("/servers", get(servers::get).post(servers::post))
        .route(
            "/servers/{organization}/{server}",
            get(servers::get_server).delete(servers::delete_server),
        )
        .route(
            "/servers/{organization}/{server}/check-connection",
            post(servers::post_server_check_connection),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize,
        ))
        .route("/auth/basic/login", post(auth::post_basic_login))
        .route("/auth/basic/register", post(auth::post_basic_register))
        .route(
            "/auth/oauth/authorize",
            get(auth::get_oauth_authorize).post(auth::post_oauth_authorize),
        )
        .route("/auth/logout", post(auth::post_logout))
        .route("/health", get(get_health));

    let app = Router::new()
        .nest("/api/v1", api)
        .fallback(handle_404)
        .layer(cors)
        .layer(trace)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await
}
