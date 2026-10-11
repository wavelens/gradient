/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::NAR_UPLOAD_CHUNK_LIMIT;
use crate::api_spec::{BEARER, enveloped, secured};
use crate::endpoints::membership::{CacheOwner, ProjectOwner};
use crate::endpoints::{self, admin, *};
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, patch, post, put};
use gradient_core::ServerState;
use gradient_types::RuntimeConfig;
use std::sync::Arc;
use utoipa_axum::router::OpenApiRouter;

pub(crate) struct BodyLimits {
    pub source_upload: usize,
}

impl BodyLimits {
    pub(crate) fn from_config(config: &RuntimeConfig) -> Self {
        Self {
            source_upload: config.http.max_source_upload_size.get(),
        }
    }

    pub(crate) fn for_spec() -> Self {
        Self { source_upload: 0 }
    }
}

pub(crate) fn auth_api(limits: &BodyLimits) -> OpenApiRouter<Arc<ServerState>> {
    secured(
        enveloped(
            OpenApiRouter::new()
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
                    post(membership::members::add::<ProjectOwner>)
                        .patch(membership::members::change_role::<ProjectOwner>)
                        .delete(membership::members::remove::<ProjectOwner>),
                )
                .route(
                    "/projects/{project}/invitations",
                    get(membership::invitations::list::<ProjectOwner>)
                        .delete(membership::invitations::revoke::<ProjectOwner>),
                )
                .route(
                    "/projects/{project}/roles",
                    get(membership::roles::list::<ProjectOwner>)
                        .post(membership::roles::create::<ProjectOwner>),
                )
                .route(
                    "/projects/{project}/roles/{role_id}",
                    get(membership::roles::show::<ProjectOwner>)
                        .patch(membership::roles::update::<ProjectOwner>)
                        .delete(membership::roles::delete::<ProjectOwner>),
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
                    "/teams/{team}/evaluations",
                    get(endpoints::teams::management::get_team_evaluations),
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
                    "/teams/{team}/workers",
                    get(endpoints::teams::workers::get_team_workers)
                        .post(endpoints::teams::workers::post_team_worker),
                )
                .route(
                    "/teams/{team}/workers/{worker_id}",
                    patch(endpoints::teams::workers::patch_team_worker)
                        .delete(endpoints::teams::workers::delete_team_worker),
                )
                .route(
                    "/projects/{project}/teams",
                    post(endpoints::teams::grants::post_project_team),
                )
                .route(
                    "/projects/{project}/teams/{team}",
                    patch(endpoints::teams::grants::patch_project_team)
                        .delete(endpoints::teams::grants::delete_project_team),
                )
                .route(
                    "/caches/{cache}/teams",
                    get(endpoints::teams::grants::get_cache_teams)
                        .post(endpoints::teams::grants::post_cache_team),
                )
                .route(
                    "/caches/{cache}/teams/{team}",
                    patch(endpoints::teams::grants::patch_cache_team)
                        .delete(endpoints::teams::grants::delete_cache_team),
                )
                .route(
                    "/teams/{team}/grants",
                    get(endpoints::teams::requests::get_team_grants),
                )
                .route(
                    "/teams/{team}/requests",
                    get(endpoints::teams::requests::get_team_requests),
                )
                .route(
                    "/teams/{team}/requests/{request_id}",
                    post(endpoints::teams::requests::post_approve_team_request)
                        .delete(endpoints::teams::requests::delete_team_request),
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
                    tasks::flake_inputs::router().into(),
                )
                .nest(
                    "/tasks/{project}/{task}/triggers",
                    tasks::triggers::router(),
                )
                .nest(
                    "/tasks/{project}/{task}/actions",
                    tasks::actions::router().into(),
                )
                .nest(
                    "/projects/{project}/webhooks",
                    endpoints::webhooks::router().into(),
                )
                .nest(
                    "/caches/{cache}/webhooks",
                    endpoints::webhooks::router().into(),
                )
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
                    "/evals/{evaluation}/retry",
                    post(evals::post_evaluation_retry),
                )
                .route(
                    "/builds/{build}/prioritize",
                    post(builds::post_build_prioritize),
                )
                .route("/builds/{build}/abort", post(builds::post_build_abort))
                .route("/builds/{build}/retry", post(builds::post_build_retry))
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
                    post(build_requests::source::post_source)
                        .layer(DefaultBodyLimit::max(limits.source_upload)),
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
                    get(membership::roles::list::<CacheOwner>)
                        .post(membership::roles::create::<CacheOwner>),
                )
                .route(
                    "/caches/{cache}/roles/{role_id}",
                    get(membership::roles::show::<CacheOwner>)
                        .patch(membership::roles::update::<CacheOwner>)
                        .delete(membership::roles::delete::<CacheOwner>),
                )
                .route(
                    "/caches/{cache}/members",
                    get(membership::members::list::<CacheOwner>)
                        .post(membership::members::add::<CacheOwner>)
                        .patch(membership::members::change_role::<CacheOwner>)
                        .delete(membership::members::remove::<CacheOwner>),
                )
                .route(
                    "/caches/{cache}/invitations",
                    get(membership::invitations::list::<CacheOwner>)
                        .delete(membership::invitations::revoke::<CacheOwner>),
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
                .nest("/admin", admin::admin_router().into()),
        ),
        BEARER,
    )
}
