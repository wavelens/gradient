/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[macro_use]
pub mod patch;

pub mod access;
pub mod audit;
pub mod authorization;
pub(crate) mod client_ip;
pub mod endpoints;
pub mod error;
pub mod helpers;
pub mod permissions;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, MatchedPath};
use axum::routing::{get, patch, post, put};
use axum::{Router, middleware};
use bytes::Bytes;
use governor::middleware::NoOpMiddleware;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, Request, Response};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tower_governor::GovernorLayer;
use tower_governor::errors::GovernorError;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::{KeyExtractor, SmartIpKeyExtractor};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;
use tracing::Span;
use uuid::Uuid;

use endpoints::{admin, *};
use gradient_core::types::ServerState;
use proto::handler::PerIpLimiter;
use proto::{ProtoLimiter, proto_router};
use scheduler::Scheduler;
use std::sync::Arc;

/// Wraps `SmartIpKeyExtractor` with a constant fallback so requests that
/// carry no client-IP signal at all (no `X-Forwarded-For` / `X-Real-IP`,
/// no `ConnectInfo`) share a single bucket instead of returning 500. This
/// matters in tests (axum-test has no peer socket) and as a defensive
/// fallback if `into_make_service_with_connect_info` is ever skipped - a
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

/// Generates an x-request-id header for incoming requests that don't carry
/// one. Using UUID v7 keeps ids monotonic per source so log scans stay
/// roughly time-ordered. Trusted upstream proxies that already inject an
/// `x-request-id` are passed through unchanged by `SetRequestIdLayer`.
#[derive(Debug, Clone, Copy, Default)]
struct MakeRequestUuid;

impl MakeRequestId for MakeRequestUuid {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7().to_string();
        // `Uuid::now_v7().to_string()` produces ASCII hex+hyphens, always a
        // valid header value - `from_str` cannot realistically fail.
        let value = HeaderValue::from_str(&id).ok()?;
        Some(RequestId::new(value))
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
        .config
        .server
        .serve_url
        .clone()
        .try_into()
        .expect("invalid serve_url");
    let debug_url: http::HeaderValue = format!("http://{}:8000", state.config.server.ip.clone())
        .try_into()
        .expect("invalid debug_url");

    let cors_allow_origin = AllowOrigin::list(vec![serve_url, debug_url]);

    let cors = CorsLayer::new()
        .allow_origin(cors_allow_origin)
        .allow_headers(vec![AUTHORIZATION, ACCEPT, CONTENT_TYPE])
        .allow_credentials(true);

    // Build one span per request, populated with method, route pattern, and
    // the request-id assigned by `SetRequestIdLayer`. All `tracing` events
    // emitted while the request is in flight - handler logs, DB queries,
    // and any task spawned via `Shutdown::spawn` (which inherits the
    // current span) - are linked to the same id, so a single grep finds
    // every line for one request.
    let trace = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<Body>| {
            let request_id = request
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            // `MatchedPath` carries the route pattern (e.g. `/orgs/{organization}`)
            // rather than the concrete path, so spans group by route in dashboards
            // and don't blow up cardinality with concrete ids.
            let route = request
                .extensions()
                .get::<MatchedPath>()
                .map(MatchedPath::as_str)
                .unwrap_or_else(|| request.uri().path());
            tracing::info_span!(
                "http_request",
                method = %request.method(),
                route = %route,
                request_id = %request_id,
            )
        })
        .on_request(|request: &Request<Body>, _span: &Span| {
            tracing::debug!(
                path = request.uri().path(),
                "request started",
            )
        })
        .on_response(
            |_response: &Response<Body>, latency: Duration, _span: &Span| {
                tracing::debug!(latency_ms = latency.as_millis() as u64, "response generated")
            },
        )
        .on_body_chunk(|chunk: &Bytes, _latency: Duration, _span: &Span| {
            tracing::debug!(bytes = chunk.len(), "sending chunk")
        })
        .on_eos(
            |_trailers: Option<&HeaderMap>, stream_duration: Duration, _span: &Span| {
                tracing::debug!(
                    duration_ms = stream_duration.as_millis() as u64,
                    "stream closed",
                )
            },
        )
        .on_failure(
            |error: ServerErrorsFailureClass, _latency: Duration, _span: &Span| {
                tracing::debug!(error = ?error, "request failed")
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
            "/orgs/{organization}/roles",
            get(orgs::get_organization_roles).post(orgs::post_organization_role),
        )
        .route(
            "/orgs/{organization}/roles/{role_id}",
            get(orgs::get_organization_role)
                .patch(orgs::patch_organization_role)
                .delete(orgs::delete_organization_role),
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
            "/orgs/{organization}/integrations/summary",
            get(orgs::get_integration_summaries),
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
        .nest(
            "/projects/{organization}/{project}/flake-inputs",
            projects::flake_inputs::router(),
        )
        .nest(
            "/projects/{organization}/{project}/triggers",
            projects::triggers::router(),
        )
        .nest(
            "/projects/{organization}/{project}/actions",
            projects::actions::router(),
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
            "/build-requests/manifest",
            post(build_requests::manifest::post_manifest),
        )
        .route(
            "/build-requests/{session}/blobs",
            post(build_requests::blobs::post_blobs).layer(DefaultBodyLimit::max(
                gradient_core::constants::MAX_BUILD_REQUEST_SIZE,
            )),
        )
        .route(
            "/build-requests/{session}/dispatch",
            post(build_requests::dispatch::post_dispatch),
        )
        .route("/caches", get(caches::get).put(caches::put))
        .route("/caches/available", get(caches::get_cache_name_available))
        .route(
            "/caches/{cache}",
            patch(caches::patch_cache).delete(caches::delete_cache),
        )
        .route(
            "/caches/{cache}/nars",
            post(caches::nars_upload)
                .layer(DefaultBodyLimit::max(state.config.limits.max_nar_upload_size)),
        )
        .route(
            "/caches/{cache}/nars/{hash}",
            axum::routing::delete(caches::nars_delete),
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
        .route(
            "/caches/{cache}/roles",
            get(caches::roles::get_cache_roles).post(caches::roles::post_cache_role),
        )
        .route(
            "/caches/{cache}/roles/{role_id}",
            get(caches::roles::get_cache_role)
                .patch(caches::roles::patch_cache_role)
                .delete(caches::roles::delete_cache_role),
        )
        .route(
            "/caches/{cache}/members",
            get(caches::members::get_cache_members)
                .post(caches::members::post_cache_member)
                .patch(caches::members::patch_cache_member)
                .delete(caches::members::delete_cache_member),
        )
        .route("/user", get(user::get).delete(user::delete))
        .route("/user/search", get(user::get_search))
        .route(
            "/user/keys",
            get(user::get_keys)
                .post(user::post_keys)
                .delete(user::delete_keys),
        )
        .route("/user/keys/permissions", get(user::get_key_permissions))
        .route("/user/keys/{api_id}", patch(user::patch_key))
        .route("/user/keys/{api_id}/revoke", post(user::post_key_revoke))
        .route("/user/sessions", get(user::get_sessions))
        .route(
            "/user/sessions/{session_id}",
            axum::routing::delete(user::delete_session),
        )
        .route("/user/audit-log", get(user::get_audit_log))
        .route(
            "/user/settings",
            get(user::get_settings).patch(user::patch_settings),
        )
        .route("/auth/cli/info", get(auth::get_cli_device_info))
        .route("/auth/cli/authorize", post(auth::post_cli_device_authorize))
        .route("/auth/cli/deny", post(auth::post_cli_device_deny))
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
        .route("/evals/{evaluation}/artefacts", get(evals::get_artefacts))
        .route("/evals/{evaluation}/closure", get(builds::get_eval_closure))
        .route(
            "/evals/{evaluation}/runtime-closure",
            get(builds::get_eval_runtime_closure),
        )
        .route("/builds/{build}", get(builds::get_build))
        .route("/builds/{build}/log", get(builds::get_build_log))
        .route(
            "/builds/{build}/log/chunks",
            get(builds::get_build_log_chunks),
        )
        .route(
            "/builds/{build}/log/chunk/{index}",
            get(builds::get_build_log_chunk),
        )
        .route("/builds/{build}/log/lines", get(builds::get_build_log_lines))
        .route(
            "/builds/{build}/log/search",
            get(builds::get_build_log_search),
        )
        .route(
            "/builds/{build}/dependencies",
            get(builds::get_build_dependencies),
        )
        .route("/builds/{build}/graph", get(builds::get_build_graph))
        .route("/builds/{build}/closure", get(builds::get_build_closure))
        .route(
            "/builds/{build}/runtime-closure",
            get(builds::get_build_runtime_closure),
        )
        .route(
            "/builds/{build}/downloads",
            get(builds::get_build_downloads),
        )
        .route(
            "/builds/{build}/download/{filename}",
            get(builds::get_build_download),
        )
        .route("/commits/{commit}", get(commits::get_commit))
        .route("/caches/{cache}", get(caches::get_cache))
        .route(
            "/caches/{cache}/public-key",
            get(caches::get_cache_public_key),
        )
        .route("/caches/{cache}/stats", get(stats::get_cache_stats))
        .route("/caches/{cache}/nars", get(caches::nars_list))
        .route("/caches/{cache}/nars/stats", get(caches::nars_stats))
        .route(
            "/caches/{cache}/nars/available",
            get(caches::nars_available),
        )
        .route("/caches/{cache}/nars/{hash}", get(caches::nars_show))
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
        .route("/auth/cli/start", post(auth::post_cli_device_start))
        .route("/auth/cli/poll", post(auth::post_cli_device_poll))
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
        // GitHub App manifest callback - unauthenticated because GitHub's
        // top-level browser redirect carries no bearer token; CSRF is bound
        // via the one-shot manifest state token issued on /admin/github-app/manifest.
        .route(
            "/admin/github-app/callback",
            get(admin::github_app::callback),
        );

    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    scheduler.start();
    proto::outbound::start_outbound_loop(Arc::clone(&scheduler));

    let proto_limiter = Arc::new(ProtoLimiter::new(state.config.proto.max_proto_connections));

    // Default tier covers everything left under /api/v1 (the bulk authenticated
    // surface) plus the proto WS upgrade.
    let api = api.route_layer(GovernorLayer::new(rl_per_ms(200, 150)));
    let api = api.layer(DefaultBodyLimit::max(state.config.limits.max_request_size));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .merge(proto_router().route_layer(GovernorLayer::new(rl_per_ms(200, 150))))
        .layer(axum::Extension(Arc::clone(&scheduler)))
        .layer(axum::Extension(Arc::clone(&proto_limiter)));

    // Metrics endpoint - root-mounted, only when an operator-configured
    // bearer token is present. Uses the same rate-limit tier as
    // auth_sensitive (6 r/s, burst 5).
    if state.config.metrics.is_some() {
        let metrics_route = Router::new()
            .route("/metrics", get(endpoints::metrics::get_metrics))
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                endpoints::metrics::metrics_auth,
            ))
            .route_layer(GovernorLayer::new(rl_per_second(6, 5)))
            .layer(axum::Extension(Arc::clone(&scheduler)));
        app = app.merge(metrics_route);
    }

    // Public NAR cache surface - substituters issue many requests per build,
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

    let cache_inspect = Router::new()
        .route("/cache/{cache}/ls/{hash}", get(caches::ls))
        .route("/cache/{cache}/serve/{hash}/{*path}", get(caches::serve))
        .route_layer(GovernorLayer::new(rl_per_second(1, 60)));

    let cache_log = Router::new()
        .route("/cache/{cache}/log/{drv}", get(caches::log))
        .route_layer(GovernorLayer::new(rl_per_second(1, 300)));

    // Cache-scoped read-only proto WebSocket. `authorize_optional` populates
    // MaybeUser/MaybeApiKey/ClientIp so the handler can authorize anon→public
    // and key→private (respecting cache_pin) and cap anonymous fan-out per IP.
    let cache_per_ip = Arc::new(PerIpLimiter::new(
        state.config.proto.anon_max_connections_per_ip,
    ));
    let cache_proto_route = Router::new()
        .route("/cache/{cache}/proto", get(caches::cache_proto))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize_optional,
        ))
        .route_layer(GovernorLayer::new(rl_per_second(
            state.config.proto.anon_rate_per_second as u64,
            state.config.proto.anon_rate_burst,
        )))
        .layer(axum::Extension(cache_per_ip))
        .layer(axum::Extension(Arc::clone(&proto_limiter)));

    app = app
        .merge(cache_routes)
        .merge(cache_inspect)
        .merge(cache_log)
        .merge(cache_proto_route);

    // Layer order (outer → inner, i.e. last `.layer()` is outermost):
    //   SetRequestIdLayer    - assigns x-request-id on inbound requests
    //   TraceLayer           - opens the span (reads the id from headers)
    //   PropagateRequestIdLayer - copies the id onto the response
    //   CORS                 - innermost so preflights still get traced
    app.fallback(handle_404)
        .layer(cors)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(trace)
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

pub async fn serve_web(state: Arc<ServerState>) -> std::io::Result<()> {
    let server_url = format!(
        "{}:{}",
        state.config.server.ip.clone(),
        state.config.server.port.clone()
    );

    // Free the partial unique index from any admin_task left in Pending/Running
    // by a previous process. Sweeps are idempotent so the operator can re-issue.
    match gradient_core::db::admin_tasks::mark_all_active_failed(&state.worker_db).await {
        Ok(n) if n > 0 => tracing::warn!(
            tasks_marked_failed = n,
            "marked stale admin tasks Failed (server restart)"
        ),
        Ok(_) => {}
        Err(e) => tracing::error!(error = ?e, "failed to clear stale admin tasks"),
    }

    let app = create_router(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind(&server_url)
        .await
        .map_err(|e| {
            tracing::error!(addr = %server_url, error = %e, "Failed to bind listener");
            e
        })?;

    let shutdown = state.shutdown.clone();
    install_signal_handler(shutdown.clone());

    let drain_token = shutdown.token();
    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move { drain_token.cancelled().await })
    .await;

    // Drain background tasks (dispatch loops, outbound, cache GC, action
    // deliveries, metric writes). Bounded so a misbehaving task can't block
    // shutdown indefinitely.
    shutdown
        .cancel_and_drain(std::time::Duration::from_secs(30))
        .await;

    serve_result
}

/// Install a SIGINT/SIGTERM handler that triggers graceful shutdown.
fn install_signal_handler(shutdown: gradient_core::shutdown::Shutdown) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "failed to install SIGTERM handler");
                    return;
                }
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT, shutting down"),
                _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("received Ctrl-C, shutting down");
        }
        shutdown.cancel();
    });
}
