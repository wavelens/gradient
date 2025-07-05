/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod authorization;
pub mod endpoints;
pub mod error;

use axum::body::Body;
use axum::routing::{get, post};
use axum::{Router, middleware};
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
    let cors_allow_origin = if state.cli.debug {
        AllowOrigin::list(vec![
            state.cli.serve_url.clone().try_into().unwrap(),
            format!("http://{}:8000", state.cli.ip.clone(),)
                .try_into()
                .unwrap(),
        ])
    } else {
        AllowOrigin::exact(state.cli.serve_url.clone().try_into().unwrap())
    };

    let cors = CorsLayer::new()
        .allow_origin(cors_allow_origin)
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
        .route("/orgs", get(orgs::get).put(orgs::put))
        .route(
            "/orgs/{organization}",
            get(orgs::get_organization)
                .patch(orgs::patch_organization)
                .delete(orgs::delete_organization),
        )
        .route(
            "/orgs/{organization}/users",
            get(orgs::get_organization_users)
                .post(orgs::post_organization_users)
                .patch(orgs::patch_organization_users)
                .delete(orgs::delete_organization_users),
        )
        .route(
            "/orgs/{organization}/ssh",
            get(orgs::get_organization_ssh).post(orgs::post_organization_ssh),
        )
        .route(
            "/orgs/{organization}/subscribe",
            get(orgs::get_organization_subscribe),
        )
        .route(
            "/orgs/{organization}/subscribe/{cache}",
            post(orgs::post_organization_subscribe_cache)
                .delete(orgs::delete_organization_subscribe_cache),
        )
        .route(
            "/projects/{organization}",
            get(projects::get).put(projects::put),
        )
        .route(
            "/projects/{organization}/{project}",
            get(projects::get_project)
                .patch(projects::patch_project)
                .delete(projects::delete_project),
        )
        .route(
            "/projects/{organization}/{project}/details",
            get(projects::get_project_details),
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
            "/projects/{organization}/{project}/active",
            post(projects::post_project_active).delete(projects::delete_project_active),
        )
        .route(
            "/evals/{evaluation}",
            get(evals::get_evaluation).post(evals::post_evaluation),
        )
        .route(
            "/evals/{evaluation}/builds",
            get(evals::get_evaluation_builds).post(evals::post_evaluation_builds),
        )
        .route("/caches", get(caches::get).put(caches::put))
        .route(
            "/caches/{cache}",
            get(caches::get_cache)
                .patch(caches::patch_cache)
                .delete(caches::delete_cache),
        )
        .route(
            "/caches/{cache}/active",
            post(caches::post_cache_active).delete(caches::delete_cache_active),
        )
        .route("/caches/{cache}/key", get(caches::get_cache_key))
        .route(
            "/builds/{build}",
            get(builds::get_build).post(builds::post_build),
        )
        .route("/builds/{build}/downloads", get(builds::get_build_downloads))
        .route("/builds/{build}/download/{filename}", get(builds::get_build_download))
        .route("/builds", post(builds::post_direct_build))
        .route("/builds/direct/recent", get(builds::get_recent_direct_builds))
        .route("/user", get(user::get).delete(user::delete))
        .route(
            "/user/keys",
            get(user::get_keys)
                .post(user::post_keys)
                .delete(user::delete_keys),
        )
        .route(
            "/user/settings",
            get(user::get_settings).patch(user::patch_settings),
        )
        .route(
            "/servers/{organization}",
            get(servers::get).put(servers::put),
        )
        .route(
            "/servers/{organization}/{server}",
            get(servers::get_server)
                .patch(servers::patch_server)
                .delete(servers::delete_server),
        )
        .route(
            "/servers/{organization}/{server}/check-connection",
            post(servers::post_server_check_connection),
        )
        .route(
            "/servers/{organization}/{server}/active",
            post(servers::post_server_active).delete(servers::delete_server_active),
        )
        .route("/commits/{commit}", get(commits::get_commit))
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
        .route("/auth/oidc/login", get(auth::get_oidc_login))
        .route("/auth/oidc/callback", get(auth::get_oidc_callback))
        .route("/auth/logout", post(auth::post_logout))
        .route("/health", get(get_health));

    let app = Router::new()
        .nest("/api/v1", api)
        .route("/cache/{cache}/nix-cache-info", get(caches::nix_cache_info))
        .route("/cache/{cache}/{path}", get(caches::path))
        .route("/cache/{cache}/nar/{path}", get(caches::nar))
        .fallback(handle_404)
        .layer(cors)
        .layer(trace)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url)
        .await
        .map_err(|e| {
            tracing::error!("Failed to bind to {}: {}", server_url, e);
            e
        })?;
    axum::serve(listener, app).await
}
