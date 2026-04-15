/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod authorization;
pub mod endpoints;
pub mod error;

use axum::body::Body;
use axum::routing::{delete, get, patch, post, put};
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
use endpoints::{workers, *};
use proto::proto_router;
use scheduler::Scheduler;
use std::sync::Arc;

/// Build the Axum router with all routes and middleware layered on.
///
/// Extracted from `serve_web` so integration tests can drive the router via
/// `axum_test::TestServer` without binding a real TCP port.
pub fn create_router(state: Arc<ServerState>) -> Router {
    let serve_url: http::HeaderValue = state
        .cli
        .serve_url
        .clone()
        .try_into()
        .expect("invalid serve_url");
    let debug_url: http::HeaderValue = format!("http://{}:8000", state.cli.ip.clone())
        .try_into()
        .expect("invalid debug_url");

    let cors_allow_origin = AllowOrigin::list(vec![serve_url, debug_url]);

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

    // ── Routes that always require a valid session ────────────────────────────
    let auth_api = Router::new()
        .route("/orgs", get(orgs::get).put(orgs::put))
        .route("/orgs/available", get(orgs::get_org_name_available))
        .route(
            "/orgs/{organization}",
            patch(orgs::patch_organization).delete(orgs::delete_organization),
        )
        .route(
            "/orgs/{organization}/public",
            post(orgs::post_organization_public).delete(orgs::delete_organization_public),
        )
        .route(
            "/orgs/{organization}/users",
            post(orgs::post_organization_users)
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
            "/orgs/{organization}/forge-webhook-secret",
            post(forge_hooks::post_forge_webhook_secret)
                .delete(forge_hooks::delete_forge_webhook_secret),
        )
        .route(
            "/orgs/{organization}/workers",
            get(orgs::get_org_workers).post(orgs::post_org_worker),
        )
        .route(
            "/orgs/{organization}/workers/{worker_id}",
            patch(orgs::patch_org_worker).delete(orgs::delete_org_worker),
        )
        .route("/projects/{organization}", put(projects::put))
        .route(
            "/projects/{organization}/available",
            get(projects::get_project_name_available),
        )
        .route(
            "/projects/{organization}/{project}",
            patch(projects::patch_project).delete(projects::delete_project),
        )
        .route(
            "/projects/{organization}/{project}/transfer",
            post(projects::post_project_transfer),
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
            "/projects/{organization}/{project}/integration",
            delete(projects::delete_project_integration),
        )
        .route("/evals/{evaluation}", post(evals::post_evaluation))
        .route(
            "/evals/{evaluation}/builds",
            post(evals::post_evaluation_builds),
        )
        .route("/builds/{build}/log", post(builds::post_build_log))
        .route(
            "/builds/{build}/download-token",
            get(builds::get_build_download_token),
        )
        .route("/builds", post(builds::post_direct_build))
        .route(
            "/builds/direct/recent",
            get(builds::get_recent_direct_builds),
        )
        .route("/caches", get(caches::get).put(caches::put))
        .route("/caches/available", get(caches::get_cache_name_available))
        .route(
            "/caches/{cache}",
            patch(caches::patch_cache).delete(caches::delete_cache),
        )
        .route(
            "/caches/{cache}/active",
            post(caches::post_cache_active).delete(caches::delete_cache_active),
        )
        .route(
            "/caches/{cache}/public",
            post(caches::post_cache_public).delete(caches::delete_cache_public),
        )
        .route("/caches/{cache}/key", get(caches::get_cache_key))
        .route("/caches/{cache}/netrc", get(caches::get_cache_netrc))
        .route(
            "/caches/{cache}/upstreams",
            get(caches::get_cache_upstreams).put(caches::put_cache_upstream),
        )
        .route(
            "/caches/{cache}/upstreams/{id}",
            patch(caches::patch_cache_upstream).delete(caches::delete_cache_upstream),
        )
        .route("/user", get(user::get).delete(user::delete))
        .route("/user/search", get(user::get_search))
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
        .route("/workers", get(workers::get_workers))
        .route("/commits/{commit}", get(commits::get_commit))
        .route(
            "/webhook/{organization}",
            get(webhooks::get).put(webhooks::put),
        )
        .route(
            "/webhook/{organization}/{webhook}",
            get(webhooks::get_webhook)
                .patch(webhooks::patch_webhook)
                .delete(webhooks::delete_webhook),
        )
        .route(
            "/webhook/{organization}/{webhook}/test",
            post(webhooks::post_webhook_test),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize,
        ));

    // ── Routes that accept optional auth (public resources browsable without login) ──
    let optional_api = Router::new()
        .route("/orgs/{organization}", get(orgs::get_organization))
        .route(
            "/orgs/{organization}/users",
            get(orgs::get_organization_users),
        )
        .route("/projects/{organization}", get(projects::get))
        .route(
            "/projects/{organization}/{project}",
            get(projects::get_project),
        )
        .route(
            "/projects/{organization}/{project}/evaluations",
            get(projects::get_project_evaluations),
        )
        .route(
            "/projects/{organization}/{project}/details",
            get(projects::get_project_details),
        )
        .route(
            "/projects/{organization}/{project}/entry-points",
            get(projects::get_project_entry_points),
        )
        .route(
            "/projects/{organization}/{project}/metrics",
            get(projects::get_project_metrics),
        )
        .route(
            "/projects/{organization}/{project}/entry-point-metrics",
            get(projects::get_entry_point_metrics),
        )
        .route(
            "/projects/{organization}/{project}/entry-point-downloads",
            get(projects::get_entry_point_download),
        )
        .route(
            "/projects/{organization}/{project}/badge",
            get(badges::get_project_badge),
        )
        .route("/evals/{evaluation}", get(evals::get_evaluation))
        .route(
            "/evals/{evaluation}/messages",
            get(evals::get_evaluation_messages),
        )
        .route(
            "/evals/{evaluation}/builds",
            get(evals::get_evaluation_builds),
        )
        .route("/builds/{build}", get(builds::get_build))
        .route("/builds/{build}/log", get(builds::get_build_log))
        .route(
            "/builds/{build}/dependencies",
            get(builds::get_build_dependencies),
        )
        .route("/builds/{build}/graph", get(builds::get_build_graph))
        .route(
            "/builds/{build}/downloads",
            get(builds::get_build_downloads),
        )
        .route(
            "/builds/{build}/download/{filename}",
            get(builds::get_build_download),
        )
        .route("/caches/{cache}", get(caches::get_cache))
        .route(
            "/caches/{cache}/public-key",
            get(caches::get_cache_public_key),
        )
        .route("/caches/{cache}/stats", get(stats::get_cache_stats))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize_optional,
        ));

    let api = Router::new()
        .merge(auth_api)
        .merge(optional_api)
        // ── Fully public (no auth required) ─────────────────────────────────
        .route("/orgs/public", get(orgs::get_public_organizations))
        .route("/caches/public", get(caches::get_public_caches))
        .route("/auth/basic/login", post(auth::post_basic_login))
        .route("/auth/basic/register", post(auth::post_basic_register))
        .route("/auth/check-username", post(auth::post_check_username))
        .route("/auth/verify-email", get(auth::get_verify_email))
        .route(
            "/auth/resend-verification",
            post(auth::post_resend_verification),
        )
        .route(
            "/auth/oauth/authorize",
            get(auth::get_oauth_authorize).post(auth::post_oauth_authorize),
        )
        .route("/auth/oidc/login", get(auth::get_oidc_login))
        .route("/auth/oidc/callback", get(auth::get_oidc_callback))
        .route("/auth/logout", post(auth::post_logout))
        .route("/health", get(get_health))
        .route("/config", get(get_config))
        // ── Incoming forge webhooks (unauthenticated, HMAC-verified) ─────────
        .route("/hooks/github", post(forge_hooks::github_app_webhook))
        .route("/hooks/{forge}/{org}", post(forge_hooks::forge_webhook));

    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    scheduler.start();
    proto::outbound::start_outbound_loop(Arc::clone(&scheduler));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .merge(proto_router())
        .layer(axum::Extension(scheduler));

    if state.cli.serve_cache {
        app = app
            .route(
                "/cache/{cache}/gradient-cache-info",
                get(caches::gradient_cache_info),
            )
            .route("/cache/{cache}/nix-cache-info", get(caches::nix_cache_info))
            .route("/cache/{cache}/{path}", get(caches::path))
            .route(
                "/cache/{cache}/nar/upstream/{upstream_id}/{*path}",
                get(caches::upstream_nar),
            )
            .route("/cache/{cache}/nar/{path}", get(caches::nar));
    }

    app.fallback(handle_404)
        .layer(cors)
        .layer(trace)
        .with_state(state)
}

pub async fn serve_web(state: Arc<ServerState>) -> std::io::Result<()> {
    let server_url = format!("{}:{}", state.cli.ip.clone(), state.cli.port.clone());
    let app = create_router(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind(&server_url)
        .await
        .map_err(|e| {
            tracing::error!("Failed to bind to {}: {}", server_url, e);
            e
        })?;
    axum::serve(listener, app).await
}
