/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[macro_use]
pub mod patch;

pub mod authorization;
pub mod endpoints;
pub mod error;
pub mod helpers;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, patch, post, put};
use axum::{Router, middleware};
use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, Request, Response};
use std::time::Duration;
use governor::middleware::NoOpMiddleware;
use std::net::{IpAddr, Ipv4Addr};
use tower_governor::GovernorLayer;
use tower_governor::errors::GovernorError;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::{KeyExtractor, SmartIpKeyExtractor};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::Span;

use gradient_core::types::ServerState;
use endpoints::{admin, *};
use proto::proto_router;
use scheduler::Scheduler;
use std::sync::Arc;

/// Wraps `SmartIpKeyExtractor` with a constant fallback so requests that
/// carry no client-IP signal at all (no `X-Forwarded-For` / `X-Real-IP`,
/// no `ConnectInfo`) share a single bucket instead of returning 500. This
/// matters in tests (axum-test has no peer socket) and as a defensive
/// fallback if `into_make_service_with_connect_info` is ever skipped — a
/// global bucket is still better than failing requests outright.
#[derive(Debug, Clone, Copy)]
struct SmartIpOrFallback;

impl KeyExtractor for SmartIpOrFallback {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        match SmartIpKeyExtractor.extract(req) {
            Ok(ip) => Ok(ip),
            Err(_) => Ok(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
        }
    }
}

/// Per-IP token-bucket config. Refill period in seconds and burst (bucket
/// capacity). Uses `SmartIpKeyExtractor` so deployments behind a reverse
/// proxy honor `X-Forwarded-For` / `X-Real-IP`.
fn rl_per_second(
    per_second: u64,
    burst: u32,
) -> Arc<GovernorConfig<SmartIpOrFallback, NoOpMiddleware>> {
    Arc::new(
        GovernorConfigBuilder::default()
            .per_second(per_second)
            .burst_size(burst)
            .key_extractor(SmartIpOrFallback)
            .finish()
            .expect("rate-limit config valid"),
    )
}

/// Sub-second refill granularity, for tiers needing >1 req/s steady-state.
fn rl_per_ms(
    per_millisecond: u64,
    burst: u32,
) -> Arc<GovernorConfig<SmartIpOrFallback, NoOpMiddleware>> {
    Arc::new(
        GovernorConfigBuilder::default()
            .per_millisecond(per_millisecond)
            .burst_size(burst)
            .key_extractor(SmartIpOrFallback)
            .finish()
            .expect("rate-limit config valid"),
    )
}

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
            "/orgs/{organization}/workers",
            get(orgs::get_org_workers).post(orgs::post_org_worker),
        )
        .route(
            "/orgs/{organization}/workers/{worker_id}",
            patch(orgs::patch_org_worker).delete(orgs::delete_org_worker),
        )
        .route(
            "/orgs/{organization}/integrations",
            get(orgs::get_integrations).put(orgs::put_integration),
        )
        .route(
            "/orgs/{organization}/integrations/{id}",
            get(orgs::get_integration)
                .patch(orgs::patch_integration)
                .delete(orgs::delete_integration),
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
            get(projects::get_project_integration)
                .put(projects::put_project_integration)
                .delete(projects::delete_project_integration),
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
        .route(
            "/builds",
            post(builds::post_direct_build)
                .layer(DefaultBodyLimit::max(state.cli.max_direct_build_size)),
        )
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
        .nest("/admin", admin::admin_router())
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

    // ── Sensitive auth surface (login/register/verification/oauth) ───────
    // Tight per-IP rate limit: Argon2 verification and email send are
    // expensive enough that an unthrottled attacker can DoS the server.
    let auth_sensitive = Router::new()
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
        .route_layer(GovernorLayer::new(rl_per_second(6, 5)));

    // ── Incoming forge webhooks (unauthenticated, HMAC-verified) ─────────
    let webhook_routes = Router::new()
        .route("/hooks/github", post(forge_hooks::github_app_webhook))
        .route(
            "/hooks/{forge}/{org}/{integration_name}",
            post(forge_hooks::forge_webhook),
        )
        .route_layer(GovernorLayer::new(rl_per_second(1, 30)));

    let api = Router::new()
        .merge(auth_api)
        .merge(optional_api)
        .merge(auth_sensitive)
        .merge(webhook_routes)
        // ── Fully public (no auth required) ─────────────────────────────────
        .route("/orgs/public", get(orgs::get_public_organizations))
        .route("/caches/public", get(caches::get_public_caches))
        .route("/auth/logout", post(auth::post_logout))
        .route("/health", get(get_health))
        .route("/config", get(get_config))
        // GitHub App manifest callback — unauthenticated because GitHub's
        // top-level browser redirect carries no bearer token; CSRF is bound
        // via the one-shot manifest state token issued on /admin/github-app/manifest.
        .route(
            "/admin/github-app/callback",
            get(admin::github_app::callback),
        );

    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    scheduler.start();
    proto::outbound::start_outbound_loop(Arc::clone(&scheduler));

    // Default tier covers everything left under /api/v1 (the bulk authenticated
    // surface) plus the proto WS upgrade.
    let api = api.route_layer(GovernorLayer::new(rl_per_ms(200, 150)));
    let api = api.layer(DefaultBodyLimit::max(state.cli.max_request_size));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .merge(proto_router().route_layer(GovernorLayer::new(rl_per_ms(200, 150))))
        .layer(axum::Extension(scheduler));

    // Public NAR cache surface — substituters issue many requests per build,
    // so the burst is generous (1000 / 1000).
    let cache_routes = Router::new()
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
        .route("/cache/{cache}/nar/{path}", get(caches::nar))
        .route_layer(GovernorLayer::new(rl_per_ms(60, 1000)));

    app = app.merge(cache_routes);

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
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
}
