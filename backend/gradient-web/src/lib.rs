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
pub mod invite_policy;
pub mod ip_allowlist;
pub mod metrics_scope;
pub mod otlp;
pub mod permissions;
pub mod scim;

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
use gradient_core::{InitError, ServerState};
use gradient_proto::proto_router;
use gradient_scheduler::Scheduler;
use gradient_wire::{PerIpLimiter, ProtoLimiter};
use std::sync::Arc;

const NAR_UPLOAD_CHUNK_LIMIT: usize = 64 * 1024 * 1024;

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

#[derive(Debug, Clone, Copy, Default)]
struct MakeRequestUuid;

impl MakeRequestId for MakeRequestUuid {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7().to_string();
        let value = HeaderValue::from_str(&id).ok()?;
        Some(RequestId::new(value))
    }
}

fn rate_limit(
    refill: Duration,
    burst: u32,
) -> Result<Arc<GovernorConfig<SmartIpOrFallback, NoOpMiddleware>>, InitError> {
    let config = GovernorConfigBuilder::default()
        .period(refill)
        .burst_size(burst)
        .key_extractor(SmartIpOrFallback)
        .finish()
        .ok_or_else(|| InitError::NetworkConfig("invalid rate-limit configuration".into()))?;

    Ok(Arc::new(config))
}

pub fn create_router(state: Arc<ServerState>) -> Result<Router, InitError> {
    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    create_router_with_scheduler(state, scheduler)
}

pub fn create_router_with_scheduler(
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
) -> Result<Router, InitError> {
    let serve_url: http::HeaderValue =
        state
            .config
            .server
            .serve_url
            .clone()
            .try_into()
            .map_err(|_| {
                InitError::NetworkConfig(format!(
                    "invalid serve_url: {}",
                    state.config.server.serve_url
                ))
            })?;
    let debug_url: http::HeaderValue =
        format!("http://{}:8000", state.config.server.listen_addr.clone())
            .try_into()
            .map_err(|_| {
                InitError::NetworkConfig(format!(
                    "invalid debug_url from ip {}",
                    state.config.server.listen_addr
                ))
            })?;

    let cors_allow_origin = AllowOrigin::list(vec![serve_url, debug_url]);

    let cors = CorsLayer::new()
        .allow_origin(cors_allow_origin)
        .allow_headers(vec![AUTHORIZATION, ACCEPT, CONTENT_TYPE])
        .allow_credentials(true);

    let trace = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<Body>| {
            let request_id = request
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
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

    let auth_api = Router::new()
        .route("/projects", get(projects::get).put(projects::put))
        .route(
            "/projects/available",
            get(projects::get_project_name_available),
        )
        .route("/board/health", get(board_metrics::get_board_health))
        .route("/board/storage", get(board_storage::get_board_storage))
        .route(
            "/board/expensive/top-projects",
            get(board::get_top_projects_by_buildtime),
        )
        .route("/board/jobs/decisions", get(board::get_assign_decisions))
        .route(
            "/projects/{project}",
            patch(projects::patch_project).delete(projects::delete_project),
        )
        .route(
            "/projects/{project}/public",
            post(projects::post_project_public).delete(projects::delete_project_public),
        )
        .route(
            "/projects/{project}/users",
            post(projects::post_project_users)
                .patch(projects::patch_project_users)
                .delete(projects::delete_project_users),
        )
        .route(
            "/projects/{project}/invitations",
            get(projects::get_project_invitations).delete(projects::delete_project_invitation),
        )
        .route(
            "/projects/{project}/roles",
            get(projects::get_project_roles).post(projects::post_project_role),
        )
        .route(
            "/projects/{project}/roles/{role_id}",
            get(projects::get_project_role)
                .patch(projects::patch_project_role)
                .delete(projects::delete_project_role),
        )
        .route(
            "/teams",
            get(endpoints::teams::management::get_teams)
                .put(endpoints::teams::management::put_team),
        )
        .route(
            "/teams/available",
            get(endpoints::teams::management::get_team_name_available),
        )
        .route(
            "/teams/{team}",
            get(endpoints::teams::management::get_team)
                .patch(endpoints::teams::management::patch_team)
                .delete(endpoints::teams::management::delete_team),
        )
        .route(
            "/teams/{team}/members",
            get(endpoints::teams::members::get_team_members)
                .post(endpoints::teams::members::post_team_members)
                .patch(endpoints::teams::members::patch_team_members)
                .delete(endpoints::teams::members::delete_team_members),
        )
        .route(
            "/teams/{team}/invitations",
            get(endpoints::teams::invitations::get_team_invitations)
                .delete(endpoints::teams::invitations::delete_team_invitation),
        )
        .route(
            "/projects/{project}/ssh",
            get(projects::get_project_ssh).post(projects::post_project_ssh),
        )
        .route(
            "/projects/{project}/subscribe",
            get(projects::get_project_subscribe),
        )
        .route(
            "/projects/{project}/subscribe/{cache}",
            post(projects::post_project_subscribe_cache)
                .delete(projects::delete_project_subscribe_cache),
        )
        .route(
            "/projects/{project}/workers",
            get(projects::get_project_workers).post(projects::post_project_worker),
        )
        .route(
            "/projects/{project}/workers/{worker_id}",
            patch(projects::patch_project_worker).delete(projects::delete_project_worker),
        )
        .route(
            "/projects/{project}/workers/{worker_id}/metrics",
            get(projects::get_project_worker_metrics),
        )
        .route(
            "/projects/{project}/workers/{worker_id}/test",
            post(projects::post_project_worker_test),
        )
        .route(
            "/gradient-ci/connections",
            post(gradient_ci_connections::post_connection),
        )
        .route(
            "/projects/{project}/integrations",
            get(projects::get_integrations).put(projects::put_integration),
        )
        .route(
            "/projects/{project}/integrations/summary",
            get(projects::get_integration_summaries),
        )
        .route(
            "/projects/{project}/integrations/{id}",
            get(projects::get_integration)
                .patch(projects::patch_integration)
                .delete(projects::delete_integration),
        )
        .route("/tasks/{project}", put(tasks::put))
        .route(
            "/tasks/{project}/available",
            get(tasks::get_task_name_available),
        )
        .route(
            "/tasks/{project}/{task}",
            patch(tasks::patch_task).delete(tasks::delete_task),
        )
        .route(
            "/tasks/{project}/{task}/transfer",
            post(tasks::post_task_transfer),
        )
        .route(
            "/tasks/{project}/{task}/check-repository",
            post(tasks::post_task_check_repository),
        )
        .route(
            "/tasks/{project}/{task}/evaluate",
            post(tasks::post_task_evaluate),
        )
        .route(
            "/tasks/{project}/{task}/active",
            post(tasks::post_task_active).delete(tasks::delete_task_active),
        )
        .nest(
            "/tasks/{project}/{task}/flake-inputs",
            tasks::flake_inputs::router(),
        )
        .nest(
            "/tasks/{project}/{task}/triggers",
            tasks::triggers::router(),
        )
        .nest("/tasks/{project}/{task}/actions", tasks::actions::router())
        .nest(
            "/projects/{project}/webhooks",
            endpoints::webhooks::router(),
        )
        .nest("/caches/{cache}/webhooks", endpoints::webhooks::router())
        .route("/evals/{evaluation}", post(evals::post_evaluation))
        .route(
            "/evals/{evaluation}/builds",
            post(evals::post_evaluation_builds),
        )
        .route(
            "/evals/{evaluation}/report",
            get(evals::get_evaluation_report),
        )
        .route(
            "/evals/{evaluation}/prioritize",
            post(evals::post_evaluation_prioritize),
        )
        .route(
            "/builds/{build}/prioritize",
            post(builds::post_build_prioritize),
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
                gradient_types::constants::MAX_BUILD_REQUEST_SIZE,
            )),
        )
        .route(
            "/build-requests/{session}/dispatch",
            post(build_requests::dispatch::post_dispatch),
        )
        .route("/build-requests/url", post(build_requests::url::post_url))
        .route(
            "/build-requests/source",
            post(build_requests::source::post_source).layer(DefaultBodyLimit::max(
                state.config.http.max_source_upload_size,
            )),
        )
        .route(
            "/build-requests/source/{upload}/chunk",
            put(build_requests::source::source_chunk)
                .layer(DefaultBodyLimit::max(NAR_UPLOAD_CHUNK_LIMIT)),
        )
        .route(
            "/build-requests/source/{upload}/finalize",
            post(build_requests::source::source_finalize),
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
                .layer(DefaultBodyLimit::max(state.config.nar.max_upload_size)),
        )
        .route(
            "/caches/{cache}/nars/{hash}/chunk",
            put(caches::nar_chunk).layer(DefaultBodyLimit::max(NAR_UPLOAD_CHUNK_LIMIT)),
        )
        .route(
            "/caches/{cache}/nars/{hash}/finalize",
            post(caches::nar_finalize),
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
            "/caches/{cache}/upstream-caches",
            put(caches::put_cache_upstream),
        )
        .route(
            "/caches/{cache}/upstream-caches/{id}",
            patch(caches::patch_cache_upstream).delete(caches::delete_cache_upstream),
        )
        .route(
            "/caches/{cache}/upstream-caches/{id}/test",
            post(caches::post_cache_upstream_test),
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
        .route(
            "/caches/{cache}/invitations",
            get(caches::invitations::get_cache_invitations)
                .delete(caches::invitations::delete_cache_invitation),
        )
        .route(
            "/caches/{cache}/subscription-requests",
            get(caches::subscriptions::get_cache_subscription_requests),
        )
        .route(
            "/caches/{cache}/subscription-requests/{project}",
            post(caches::subscriptions::post_approve_subscription_request)
                .delete(caches::subscriptions::delete_subscription_request),
        )
        .route("/user", get(user::get).delete(user::delete))
        .route("/user/search", get(user::get_search))
        .route("/dashboard/tasks", get(dashboard::tasks::get_tasks))
        .route("/dashboard/stats", get(dashboard::stats::get_stats))
        .route(
            "/dashboard/activity",
            get(dashboard::activity::get_activity),
        )
        .route("/dashboard/rail", get(dashboard::rail::get_rail))
        .route("/search", get(search::get_search))
        .route("/user/stars", get(stars::get_stars))
        .route(
            "/user/stars/projects/{project}",
            put(stars::put_project_star).delete(stars::delete_project_star),
        )
        .route(
            "/user/stars/tasks/{project}/{task}",
            put(stars::put_task_star).delete(stars::delete_task_star),
        )
        .route(
            "/user/stars/caches/{cache}",
            put(stars::put_cache_star).delete(stars::delete_cache_star),
        )
        .route(
            "/user/keys",
            get(user::get_keys)
                .post(user::post_keys)
                .delete(user::delete_keys),
        )
        .route("/user/keys/permissions", get(user::get_key_permissions))
        .route(
            "/user/ssh-keys",
            get(user_ssh_keys::get_ssh_keys).post(user_ssh_keys::post_ssh_key),
        )
        .route(
            "/user/ssh-keys/{ssh_key_id}",
            axum::routing::delete(user_ssh_keys::delete_ssh_key),
        )
        .route("/user/keys/{api_id}", patch(user::patch_key))
        .route("/user/keys/{api_id}/revoke", post(user::post_key_revoke))
        .route("/user/invites", get(endpoints::invites::get_user_invites))
        .route(
            "/user/invites/accept",
            post(endpoints::invites::post_accept_invite),
        )
        .route(
            "/user/invites/decline",
            post(endpoints::invites::post_decline_invite),
        )
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
        .route("/metrics/events", get(endpoints::events::firehose_ws))
        .route("/events/catalog", get(endpoints::events::get_catalog))
        .nest("/admin", admin::admin_router())
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize,
        ));

    let optional_api = Router::new()
        .route("/projects/{project}", get(projects::get_project))
        .route(
            "/projects/{project}/users",
            get(projects::get_project_users),
        )
        .route("/tasks/{project}", get(tasks::get))
        .route("/tasks/{project}/{task}", get(tasks::get_task))
        .route(
            "/tasks/{project}/{task}/evaluations",
            get(tasks::get_task_evaluations),
        )
        .route(
            "/tasks/{project}/{task}/details",
            get(tasks::get_task_details),
        )
        .route(
            "/tasks/{project}/{task}/entry-points",
            get(tasks::get_task_entry_points),
        )
        .route(
            "/tasks/{project}/{task}/metrics",
            get(tasks::get_task_metrics),
        )
        .route(
            "/tasks/{project}/{task}/entry-point-metrics",
            get(tasks::get_entry_point_metrics),
        )
        .route(
            "/tasks/{project}/{task}/entry-point-downloads",
            get(tasks::get_entry_point_download),
        )
        .route("/tasks/{project}/{task}/badge", get(badges::get_task_badge))
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
            "/evals/{evaluation}/flake-graph",
            get(board::get_eval_flake_graph),
        )
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
        .route(
            "/builds/{build}/log/lines",
            get(builds::get_build_log_lines),
        )
        .route(
            "/builds/{build}/log/search",
            get(builds::get_build_log_search),
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
        .route(
            "/caches/{cache}/upstream-caches",
            get(caches::get_upstream_caches),
        )
        .route("/caches/{cache}/stats", get(stats::get_cache_stats))
        .route("/caches/{cache}/nars", get(caches::nars_list))
        .route("/caches/{cache}/nars/stats", get(caches::nars_stats))
        .route(
            "/caches/{cache}/nars/available",
            get(caches::nars_available),
        )
        .route("/caches/{cache}/nars/{hash}", get(caches::nars_show))
        .route("/metrics/catalog", get(metrics_query::get_metrics_catalog))
        .route("/metrics/query", get(metrics_query::get_metrics_query))
        .route(
            "/metrics/tasks/{project}/{task}/evaluations",
            get(tasks::get_task_metrics),
        )
        .route(
            "/metrics/tasks/{project}/{task}/entry-point",
            get(tasks::get_entry_point_metrics),
        )
        .route("/board/jobs/dispatched", get(board::get_dispatched_jobs))
        .route("/board/jobs/pending", get(board::get_pending_jobs))
        .route("/board/jobs/expensive", get(board::get_expensive_jobs))
        .route(
            "/board/jobs/expensive-by-resource",
            get(board::get_expensive_by_resource),
        )
        .route(
            "/board/evals/expensive-by-resource",
            get(board::get_expensive_evals_by_resource),
        )
        .route("/board/jobs/{id}", get(board::get_dispatched_job))
        .route("/board/scoring/summary", get(board::get_scoring_summary))
        .route("/board/scoring/rules", get(board::get_scoring_rules))
        .route("/board/cache", get(board_metrics::get_board_cache))
        .route(
            "/board/cache/upstream-caches",
            get(board_metrics::get_board_upstream_caches),
        )
        .route("/board/network", get(board_metrics::get_board_network))
        .route("/board/fleet", get(board_metrics::get_board_fleet))
        .route(
            "/board/durations/heatmap",
            get(board_metrics::get_board_durations_heatmap),
        )
        .route("/board/workers", get(board::get_board_workers))
        .route("/board/workers/load", get(board::get_board_worker_load))
        .route("/board/live", get(board::board_live_ws))
        .route("/board/cache/live", get(live::cache_live_ws))
        .route("/tasks/{project}/{task}/live", get(live::task_live_ws))
        .route("/evals/{evaluation}/live", get(live::evaluation_live_ws))
        .route("/builds/{build}/live", get(live::build_live_ws))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize_optional,
        ));

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
        .route_layer(GovernorLayer::new(rate_limit(Duration::from_secs(6), 5)?));

    let webhook_routes = Router::new()
        .route("/hooks/github", post(git_host_hooks::github_app_webhook))
        .route(
            "/hooks/{git_host}/{project}/{integration_name}",
            post(git_host_hooks::git_host_webhook),
        )
        .route_layer(GovernorLayer::new(rate_limit(Duration::from_secs(1), 30)?));

    let api = Router::new()
        .merge(auth_api)
        .merge(optional_api)
        .merge(auth_sensitive)
        .merge(webhook_routes)
        .route("/projects/public", get(projects::get_public_projects))
        .route("/caches/public", get(caches::get_public_caches))
        .route("/auth/logout", post(auth::post_logout))
        .route("/health", get(get_health))
        .route("/config", get(get_config))
        .route(
            "/admin/github-app/callback",
            get(admin::github_app::callback),
        );

    scheduler.start();
    state
        .shutdown
        .supervise(gradient_db::maintenance::retention::child_spec(state.db()));
    state
        .shutdown
        .supervise(gradient_db::metrics::rollup::child_spec(state.db()));
    state
        .shutdown
        .supervise(gradient_db::metrics::cache_traffic::child_spec(
            state.db(),
            Arc::clone(&state.cache_traffic),
        ));
    gradient_db::metrics::cache_traffic::flush_on_shutdown(
        state.db(),
        Arc::clone(&state.cache_traffic),
    );
    state
        .shutdown
        .supervise(gradient_db::metrics::infra::child_spec(state.db()));
    gradient_db::metrics::infra::flush_on_shutdown(state.db());
    state
        .shutdown
        .spawn(gradient_core::upstream::persist_http1_pins(
            state.web_db.clone(),
            state.shutdown.token(),
        ));
    otlp::start_otlp(Arc::clone(&state), Arc::clone(&scheduler));
    let sessions = gradient_proto::SessionsHandle::new();
    state
        .shutdown
        .supervise(sessions.child_spec(Arc::clone(&scheduler)));
    gradient_proto::outbound::start_outbound_loop(Arc::clone(&scheduler), Arc::clone(&sessions));

    let proto_limiter = Arc::new(ProtoLimiter::new(state.config.proto.max_connections));

    let api = api.route_layer(GovernorLayer::new(rate_limit(
        Duration::from_millis(200),
        150,
    )?));
    let api = api.route_layer(axum::middleware::from_fn(metrics::track_http_metrics));
    let api = api.layer(DefaultBodyLimit::max(state.config.http.max_request_size));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .merge(proto_router().route_layer(GovernorLayer::new(rate_limit(
            Duration::from_millis(200),
            150,
        )?)))
        .layer(axum::Extension(Arc::clone(&scheduler)))
        .layer(axum::Extension(Arc::clone(&proto_limiter)))
        .layer(axum::Extension(Arc::clone(&sessions)));

    if state.config.metrics.is_some() {
        let metrics_route = Router::new()
            .route("/metrics", get(endpoints::metrics::get_metrics))
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                endpoints::metrics::metrics_auth,
            ))
            .route_layer(GovernorLayer::new(rate_limit(Duration::from_secs(1), 5)?))
            .layer(axum::Extension(Arc::clone(&scheduler)));
        app = app.merge(metrics_route);
    }

    let nar_cache_limit = || rate_limit(Duration::from_millis(20), 3000);
    let cache_routes = Router::new()
        .route("/cache/{cache}", get(caches::cache_root))
        .route("/cache/{cache}/", get(caches::cache_root))
        .route(
            "/cache/{cache}/gradient-cache-info",
            get(caches::gradient_cache_info),
        )
        .route("/cache/{cache}/nix-cache-info", get(caches::nix_cache_info))
        .route(
            "/cache/{cache}/debuginfo/{build_id}",
            get(caches::debuginfo),
        )
        .route("/cache/{cache}/{path}", get(caches::path))
        .route(
            "/cache/{cache}/nar/upstream/{upstream_id}/{*path}",
            get(caches::upstream_nar),
        )
        .route("/cache/{cache}/nar/{path}", get(caches::nar))
        .route_layer(GovernorLayer::new(nar_cache_limit()?));

    let cache_inspect = Router::new()
        .route("/cache/{cache}/ls/{hash}", get(caches::ls))
        .route("/cache/{cache}/serve/{hash}/{*path}", get(caches::serve))
        .route_layer(GovernorLayer::new(rate_limit(
            Duration::from_millis(333),
            180,
        )?));

    let cache_log = Router::new()
        .route("/cache/{cache}/log/{drv}", get(caches::log))
        .route_layer(GovernorLayer::new(rate_limit(
            Duration::from_millis(333),
            900,
        )?));

    let cache_per_ip = Arc::new(PerIpLimiter::new(
        state.config.proto.anonymous_cache_max_connections_per_ip,
    ));
    let cache_proto_route = Router::new()
        .route("/cache/{cache}/proto", get(caches::cache_proto))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize_optional,
        ))
        .route_layer(GovernorLayer::new(nar_cache_limit()?))
        .layer(axum::Extension(cache_per_ip))
        .layer(axum::Extension(Arc::clone(&proto_limiter)))
        .layer(axum::Extension(Arc::clone(&sessions)));

    app = app
        .merge(cache_routes)
        .merge(cache_inspect)
        .merge(cache_log)
        .merge(cache_proto_route);

    if state.config.scim.is_some() {
        let scim_routes = Router::new()
            .route(
                "/ServiceProviderConfig",
                get(scim::discovery::service_provider_config),
            )
            .route("/ResourceTypes", get(scim::discovery::resource_types))
            .route("/Schemas", get(scim::discovery::schemas))
            .route("/Users", get(scim::users::list).post(scim::users::create))
            .route(
                "/Users/{id}",
                get(scim::users::get)
                    .put(scim::users::replace)
                    .patch(scim::users::patch)
                    .delete(scim::users::delete),
            )
            .route("/Groups", get(scim::groups::list))
            .route(
                "/Groups/{id}",
                get(scim::groups::get).patch(scim::groups::patch),
            )
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                authorization::authorize_scim,
            ));
        app = app.nest("/scim/v2", scim_routes);
    }

    Ok(app
        .fallback(handle_404)
        .layer(cors)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(trace)
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state))
}

/// Nagle is disabled on every accepted connection. It would hold a small `/proto` control frame
/// back until the peer's delayed ACK. The concrete return type is required because
/// `ConnectInfo<SocketAddr>` is resolving through an impl for `TapIo<L, F>`.
fn tuned_listener(
    listener: tokio::net::TcpListener,
) -> axum::serve::TapIo<tokio::net::TcpListener, fn(&mut tokio::net::TcpStream)> {
    use axum::serve::ListenerExt;
    fn tap(stream: &mut tokio::net::TcpStream) {
        gradient_util::net::disable_nagle(stream);
    }
    listener.tap_io(tap as fn(&mut tokio::net::TcpStream))
}

pub async fn serve_web(state: Arc<ServerState>, scheduler: Arc<Scheduler>) -> std::io::Result<()> {
    let server_url = format!(
        "{}:{}",
        state.config.server.listen_addr.clone(),
        state.config.server.port.clone()
    );

    match gradient_db::maintenance::recovery::recover_interrupted_work(&state.worker_db).await {
        Ok(r)
            if r.assignments_closed > 0
                || r.attempts_aborted > 0
                || r.builds_requeued > 0
                || r.builds_unpromoted > 0
                || r.builds_aborted > 0
                || r.evals_aborted > 0
                || r.tasks_forced > 0
                || r.cluster_attempts_closed > 0 =>
        {
            tracing::warn!(
                dispatches_closed = r.assignments_closed,
                attempts_aborted = r.attempts_aborted,
                builds_requeued = r.builds_requeued,
                builds_unpromoted = r.builds_unpromoted,
                builds_aborted = r.builds_aborted,
                evals_aborted = r.evals_aborted,
                tasks_forced = r.tasks_forced,
                cluster_attempts_closed = r.cluster_attempts_closed,
                clusters_requeued = r.clusters_requeued,
                clusters_aborted = r.clusters_aborted,
                clusters_failed = r.clusters_failed,
                "recovered interrupted work from previous process"
            )
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = ?e, "failed to recover interrupted work"),
    }

    match gradient_db::evaluations::draining::unpark_draining_evals(&state.worker_db).await {
        Ok(n) if n > 0 => {
            tracing::warn!(evaluations = n, "recovered evaluations parked by draining")
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = ?e, "failed to recover draining-parked evaluations"),
    }

    let app = create_router_with_scheduler(Arc::clone(&state), scheduler)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let listener = tokio::net::TcpListener::bind(&server_url)
        .await
        .map_err(|e| {
            tracing::error!(addr = %server_url, error = %e, "Failed to bind listener");
            e
        })?;
    let listener = tuned_listener(listener);
    sd_notify::notify(&[sd_notify::NotifyState::Ready])?;

    let shutdown = state.shutdown.clone();
    install_signal_handler(shutdown.clone());

    let drain_token = shutdown.token();
    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move { drain_token.cancelled().await })
    .await;

    shutdown
        .cancel_and_drain(std::time::Duration::from_secs(30))
        .await;

    serve_result
}

fn install_signal_handler(shutdown: gradient_util::shutdown::Shutdown) {
    let trigger = shutdown.clone();
    shutdown.spawn(async move {
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
        trigger.cancel();
    });
}

/// A linker is dropping an rlib nothing mentions, registry entries included. This function is
/// pulling the crate into binaries for the plan gate's `gradient_db::sql!` registry.
pub const fn link() {}

#[cfg(test)]
mod tests {
    use super::tuned_listener;
    use axum::serve::Listener as _;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn accepted_connections_have_nagle_disabled() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let mut listener = tuned_listener(listener);

        let (accepted, client) = tokio::join!(listener.accept(), TcpStream::connect(addr));
        let (io, _peer) = accepted;
        let _client = client.expect("connect");

        assert!(io.nodelay().expect("read nodelay"));
    }
}
