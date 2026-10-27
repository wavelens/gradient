/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::NAR_UPLOAD_CHUNK_LIMIT;
use crate::api_spec::{
    BEARER, CACHE_BASIC, METRICS, SCIM, enveloped, limited, optionally_secured, secured, unsecured,
    wildcard,
};
use crate::endpoints::webhooks::WebhookMount;
use crate::endpoints::{self, admin, *};
use crate::scim;
use axum::routing::get;
use gradient_core::ServerState;
use gradient_proto::proto_router;
use gradient_types::RuntimeConfig;
use std::sync::Arc;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub(crate) struct BodyLimits {
    pub source_upload: usize,
    pub nar_upload: usize,
}

impl BodyLimits {
    pub(crate) fn from_config(config: &RuntimeConfig) -> Self {
        Self {
            source_upload: config.http.max_source_upload_size.get(),
            nar_upload: config.nar.max_upload_size.get(),
        }
    }

    pub(crate) fn for_spec() -> Self {
        Self {
            source_upload: 0,
            nar_upload: 0,
        }
    }
}

pub(crate) fn auth_api(limits: &BodyLimits) -> OpenApiRouter<Arc<ServerState>> {
    secured(
        enveloped(
            OpenApiRouter::new()
                .routes(routes!(
                    projects::management::get,
                    projects::management::put
                ))
                .routes(routes!(projects::management::get_project_name_available))
                .routes(routes!(board_metrics::get_board_health))
                .routes(routes!(board_storage::get_board_storage))
                .routes(routes!(board::get_top_projects_by_buildtime))
                .routes(routes!(board::get_assign_decisions))
                .routes(routes!(
                    projects::management::patch_project,
                    projects::management::delete_project
                ))
                .routes(routes!(
                    projects::settings::post_project_public,
                    projects::settings::delete_project_public
                ))
                .routes(routes!(
                    membership::endpoints::project::add_member,
                    membership::endpoints::project::change_role,
                    membership::endpoints::project::remove_member
                ))
                .routes(routes!(
                    membership::endpoints::project::list_invitations,
                    membership::endpoints::project::revoke_invitation
                ))
                .routes(routes!(
                    membership::endpoints::project::list_roles,
                    membership::endpoints::project::create_role
                ))
                .routes(routes!(
                    membership::endpoints::project::show_role,
                    membership::endpoints::project::update_role,
                    membership::endpoints::project::delete_role
                ))
                .routes(routes!(
                    teams::management::get_teams,
                    teams::management::put_team
                ))
                .routes(routes!(teams::management::get_team_name_available))
                .routes(routes!(
                    teams::management::get_team,
                    teams::management::patch_team,
                    teams::management::delete_team
                ))
                .routes(routes!(teams::management::get_team_evaluations))
                .routes(routes!(
                    teams::members::get_team_members,
                    teams::members::post_team_members,
                    teams::members::patch_team_members,
                    teams::members::delete_team_members
                ))
                .routes(routes!(
                    teams::invitations::get_team_invitations,
                    teams::invitations::delete_team_invitation
                ))
                .routes(routes!(
                    teams::workers::get_team_workers,
                    teams::workers::post_team_worker
                ))
                .routes(routes!(teams::demand::get_team_worker_demand))
                .routes(routes!(
                    teams::workers::patch_team_worker,
                    teams::workers::delete_team_worker
                ))
                .routes(routes!(teams::grants::post_project_team))
                .routes(routes!(
                    teams::grants::patch_project_team,
                    teams::grants::delete_project_team
                ))
                .routes(routes!(
                    teams::grants::get_cache_teams,
                    teams::grants::post_cache_team
                ))
                .routes(routes!(
                    teams::grants::patch_cache_team,
                    teams::grants::delete_cache_team
                ))
                .routes(routes!(teams::requests::get_team_grants))
                .routes(routes!(teams::requests::get_team_requests))
                .routes(routes!(
                    teams::requests::post_approve_team_request,
                    teams::requests::delete_team_request
                ))
                .routes(routes!(
                    projects::ssh::get_project_ssh,
                    projects::ssh::post_project_ssh
                ))
                .routes(routes!(projects::settings::get_project_subscribe))
                .routes(routes!(
                    projects::settings::post_project_subscribe_cache,
                    projects::settings::delete_project_subscribe_cache
                ))
                .routes(routes!(
                    projects::workers::get_project_workers,
                    projects::workers::post_project_worker
                ))
                .routes(routes!(
                    projects::workers::patch_project_worker,
                    projects::workers::delete_project_worker
                ))
                .routes(routes!(projects::workers::get_project_worker_metrics))
                .routes(routes!(projects::workers::post_project_worker_test))
                .routes(routes!(gradient_ci_connections::post_connection))
                .routes(routes!(
                    projects::integrations::get_integrations,
                    projects::integrations::put_integration
                ))
                .routes(routes!(projects::integrations::get_integration_summaries))
                .routes(routes!(
                    projects::integrations::get_integration,
                    projects::integrations::patch_integration,
                    projects::integrations::delete_integration
                ))
                .routes(routes!(tasks::management::put))
                .routes(routes!(tasks::management::get_task_name_available))
                .routes(routes!(
                    tasks::management::patch_task,
                    tasks::management::delete_task
                ))
                .routes(routes!(tasks::management::post_task_transfer))
                .routes(routes!(tasks::management::post_task_check_repository))
                .routes(routes!(tasks::evaluations::post_task_evaluate))
                .routes(routes!(
                    tasks::management::post_task_active,
                    tasks::management::delete_task_active
                ))
                .routes(routes!(evals::actions::post_evaluation))
                .routes(routes!(evals::log::post_evaluation_builds))
                .routes(routes!(evals::report::get_evaluation_report))
                .routes(routes!(evals::actions::post_evaluation_prioritize))
                .routes(routes!(evals::actions::post_evaluation_retry))
                .routes(routes!(builds::actions::post_build_prioritize))
                .routes(routes!(builds::actions::post_build_abort))
                .routes(routes!(builds::actions::post_build_retry))
                .routes(routes!(exec_jobs::actions::post_exec_job_abort))
                .routes(routes!(builds::log::post_build_log))
                .routes(routes!(builds::downloads::get_build_download_token))
                .routes(routes!(build_requests::manifest::post_manifest))
                .routes(limited(
                    routes!(build_requests::blobs::post_blobs),
                    gradient_types::constants::MAX_BUILD_REQUEST_SIZE,
                ))
                .routes(routes!(build_requests::dispatch::post_dispatch))
                .routes(routes!(build_requests::url::post_url))
                .routes(limited(
                    routes!(build_requests::source::post_source),
                    limits.source_upload,
                ))
                .routes(limited(
                    routes!(build_requests::source::source_chunk),
                    NAR_UPLOAD_CHUNK_LIMIT,
                ))
                .routes(routes!(build_requests::source::source_finalize))
                .routes(routes!(caches::management::get, caches::management::put))
                .routes(routes!(caches::management::get_cache_name_available))
                .routes(routes!(
                    caches::management::patch_cache,
                    caches::management::delete_cache
                ))
                .routes(routes!(caches::nars::delete))
                .routes(routes!(
                    caches::management::post_cache_active,
                    caches::management::delete_cache_active
                ))
                .routes(routes!(
                    caches::management::post_cache_public,
                    caches::management::delete_cache_public
                ))
                .routes(routes!(caches::keys::get_cache_key))
                .routes(routes!(caches::upstream_caches::put_cache_upstream))
                .routes(routes!(
                    caches::upstream_caches::patch_cache_upstream,
                    caches::upstream_caches::delete_cache_upstream
                ))
                .routes(routes!(caches::upstream_caches::post_cache_upstream_test))
                .routes(routes!(
                    membership::endpoints::cache::list_roles,
                    membership::endpoints::cache::create_role
                ))
                .routes(routes!(
                    membership::endpoints::cache::show_role,
                    membership::endpoints::cache::update_role,
                    membership::endpoints::cache::delete_role
                ))
                .routes(routes!(
                    membership::endpoints::cache::list_members,
                    membership::endpoints::cache::add_member,
                    membership::endpoints::cache::change_role,
                    membership::endpoints::cache::remove_member
                ))
                .routes(routes!(
                    membership::endpoints::cache::list_invitations,
                    membership::endpoints::cache::revoke_invitation
                ))
                .routes(routes!(
                    caches::subscriptions::get_cache_subscription_requests
                ))
                .routes(routes!(
                    caches::subscriptions::post_approve_subscription_request,
                    caches::subscriptions::delete_subscription_request
                ))
                .routes(routes!(user::get, user::delete))
                .routes(routes!(user::get_search))
                .routes(routes!(dashboard::tasks::get_tasks))
                .routes(routes!(dashboard::stats::get_stats))
                .routes(routes!(dashboard::activity::get_activity))
                .routes(routes!(dashboard::rail::get_rail))
                .routes(routes!(search::get_search))
                .routes(routes!(stars::get_stars))
                .routes(routes!(stars::put_project_star, stars::delete_project_star))
                .routes(routes!(stars::put_task_star, stars::delete_task_star))
                .routes(routes!(stars::put_cache_star, stars::delete_cache_star))
                .routes(routes!(user::get_keys, user::post_keys, user::delete_keys))
                .routes(routes!(user::get_key_permissions))
                .routes(routes!(
                    user_ssh_keys::get_ssh_keys,
                    user_ssh_keys::post_ssh_key
                ))
                .routes(routes!(user_ssh_keys::delete_ssh_key))
                .routes(routes!(user::patch_key))
                .routes(routes!(user::post_key_revoke))
                .routes(routes!(invites::get_user_invites))
                .routes(routes!(invites::post_accept_invite))
                .routes(routes!(invites::post_decline_invite))
                .routes(routes!(user::get_sessions))
                .routes(routes!(user::delete_session))
                .routes(routes!(user::get_audit_log))
                .routes(routes!(user::get_settings, user::patch_settings))
                .routes(routes!(auth::get_cli_device_info))
                .routes(routes!(auth::post_cli_device_authorize))
                .routes(routes!(auth::post_cli_device_deny))
                .routes(routes!(events::firehose_ws))
                .routes(routes!(events::get_catalog))
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
                    webhooks::router(WebhookMount::Project),
                )
                .nest(
                    "/caches/{cache}/webhooks",
                    webhooks::router(WebhookMount::Cache),
                )
                .nest(
                    "/teams/{team}/webhooks",
                    webhooks::router(WebhookMount::Team),
                )
                .nest("/admin", admin::admin_router()),
        ),
        BEARER,
    )
}

pub(crate) fn optional_api() -> OpenApiRouter<Arc<ServerState>> {
    optionally_secured(
        enveloped(
            OpenApiRouter::new()
                .routes(routes!(projects::management::get_project))
                .routes(routes!(membership::endpoints::project::list_members))
                .routes(routes!(teams::grants::get_project_teams))
                .routes(routes!(tasks::management::get))
                .routes(routes!(tasks::management::get_task))
                .routes(routes!(tasks::evaluations::get_task_evaluations))
                .routes(routes!(tasks::evaluations::get_task_details))
                .routes(routes!(tasks::evaluations::get_task_entry_points))
                .routes(routes!(tasks::metrics::get_task_metrics))
                .routes(routes!(tasks::metrics::get_entry_point_metrics))
                .routes(routes!(tasks::evaluations::get_entry_point_download))
                .routes(routes!(badges::get_task_badge))
                .routes(routes!(evals::query::get_evaluation))
                .routes(routes!(evals::query::get_evaluation_messages))
                .routes(routes!(evals::query::get_evaluation_builds))
                .routes(routes!(evals::summary::get_evaluation_summary))
                .routes(routes!(evals::artefacts::get_artefacts))
                .routes(routes!(builds::closure::get_eval_closure))
                .routes(routes!(board::get_eval_flake_graph))
                .routes(routes!(builds::closure::get_eval_runtime_closure))
                .routes(routes!(builds::query::get_build))
                .routes(routes!(builds::log::get_build_log))
                .routes(routes!(builds::log_chunks::get_build_log_chunks))
                .routes(routes!(builds::log_chunks::get_build_log_chunk))
                .routes(routes!(builds::log_chunks::get_build_log_lines))
                .routes(routes!(builds::log_chunks::get_build_log_search))
                .routes(routes!(builds::graph::get_build_graph))
                .routes(routes!(builds::closure::get_build_closure))
                .routes(routes!(builds::closure::get_build_runtime_closure))
                .routes(routes!(builds::sbom::get_build_sbom))
                .routes(routes!(builds::downloads::get_build_downloads))
                .routes(routes!(builds::downloads::get_build_download))
                .routes(routes!(exec_jobs::query::get_exec_job))
                .routes(routes!(exec_jobs::log::get_exec_job_log))
                .routes(routes!(exec_jobs::query::list_evaluation_exec_jobs))
                .routes(routes!(commits::get_commit))
                .routes(routes!(caches::management::get_cache))
                .routes(routes!(caches::keys::get_cache_public_key))
                .routes(routes!(caches::upstream_caches::get_upstream_caches))
                .routes(routes!(stats::get_cache_stats))
                .routes(routes!(caches::nars::list))
                .routes(routes!(caches::nars::stats))
                .routes(routes!(caches::nars::available))
                .routes(routes!(caches::nars::show))
                .routes(routes!(metrics_query::get_metrics_catalog))
                .routes(routes!(metrics_query::get_metrics_query))
                .routes(routes!(tasks::metrics::task_evaluation_metrics))
                .routes(routes!(tasks::metrics::task_entry_point_metrics))
                .routes(routes!(board::get_dispatched_jobs))
                .routes(routes!(board::get_pending_jobs))
                .routes(routes!(board::get_expensive_jobs))
                .routes(routes!(board::get_expensive_by_resource))
                .routes(routes!(board::get_expensive_evals_by_resource))
                .routes(routes!(board::get_dispatched_job))
                .routes(routes!(board::get_scoring_summary))
                .routes(routes!(board::get_scoring_rules))
                .routes(routes!(board_metrics::get_board_cache))
                .routes(routes!(board_metrics::get_board_upstream_caches))
                .routes(routes!(board_metrics::get_board_network))
                .routes(routes!(board_metrics::get_board_fleet))
                .routes(routes!(board_metrics::get_board_durations_heatmap))
                .routes(routes!(board::get_board_workers))
                .routes(routes!(board::get_board_worker_load))
                .routes(routes!(board::board_live_ws))
                .routes(routes!(live::cache_live_ws))
                .routes(routes!(live::task_live_ws))
                .routes(routes!(live::evaluation_live_ws))
                .routes(routes!(live::build_live_ws)),
        ),
        BEARER,
    )
}

pub(crate) fn auth_sensitive() -> OpenApiRouter<Arc<ServerState>> {
    unsecured(enveloped(
        OpenApiRouter::new()
            .routes(routes!(auth::post_basic_login))
            .routes(routes!(auth::post_basic_register))
            .routes(routes!(auth::post_check_username))
            .routes(routes!(auth::get_verify_email))
            .routes(routes!(auth::post_resend_verification))
            .routes(routes!(
                auth::get_oauth_authorize,
                auth::post_oauth_authorize
            ))
            .routes(routes!(auth::get_oidc_login))
            .routes(routes!(auth::get_oidc_callback))
            .routes(routes!(auth::post_cli_device_start))
            .routes(routes!(auth::post_cli_device_poll)),
    ))
}

pub(crate) fn webhook_routes() -> OpenApiRouter<Arc<ServerState>> {
    unsecured(enveloped(
        OpenApiRouter::new()
            .routes(routes!(git_host_hooks::github_app_webhook))
            .routes(routes!(git_host_hooks::git_host_webhook)),
    ))
}

pub(crate) fn open_api() -> OpenApiRouter<Arc<ServerState>> {
    unsecured(enveloped(
        OpenApiRouter::new()
            .routes(routes!(projects::management::get_public_projects))
            .routes(routes!(caches::management::get_public_caches))
            .routes(routes!(auth::post_logout))
            .routes(routes!(endpoints::get_health))
            .routes(routes!(endpoints::get_config))
            .routes(routes!(admin::github_app::callback)),
    ))
}

pub(crate) fn cache_upload_api(limits: &BodyLimits) -> OpenApiRouter<Arc<ServerState>> {
    secured(
        enveloped(
            OpenApiRouter::new()
                .routes(limited(
                    routes!(caches::upload::nars_upload),
                    limits.nar_upload,
                ))
                .routes(limited(
                    routes!(caches::upload::nar_chunk),
                    NAR_UPLOAD_CHUNK_LIMIT,
                ))
                .routes(routes!(caches::upload::nar_finalize)),
        ),
        BEARER,
    )
}

pub(crate) fn metrics_api() -> OpenApiRouter<Arc<ServerState>> {
    secured(
        OpenApiRouter::new().routes(routes!(metrics::get_metrics)),
        METRICS,
    )
}

pub(crate) fn cache_routes() -> OpenApiRouter<Arc<ServerState>> {
    let router = OpenApiRouter::new()
        .routes(routes!(caches::narinfo::cache_root))
        .route("/cache/{cache}/", get(caches::narinfo::cache_root))
        .routes(routes!(caches::narinfo::gradient_cache_info))
        .routes(routes!(caches::narinfo::nix_cache_info))
        .routes(routes!(caches::debuginfo::debuginfo))
        .routes(routes!(caches::narinfo::path))
        .routes(routes!(caches::realisation::realisation))
        .routes(routes!(caches::nar::nar));
    optionally_secured(
        wildcard(router, routes!(caches::nar::upstream_nar)),
        CACHE_BASIC,
    )
}

pub(crate) fn cache_inspect() -> OpenApiRouter<Arc<ServerState>> {
    let router = OpenApiRouter::new().routes(routes!(caches::narlist::ls));
    optionally_secured(wildcard(router, routes!(caches::serve::serve)), CACHE_BASIC)
}

pub(crate) fn cache_log() -> OpenApiRouter<Arc<ServerState>> {
    optionally_secured(
        OpenApiRouter::new().routes(routes!(caches::build_log::log)),
        CACHE_BASIC,
    )
}

pub(crate) fn cache_proto_route() -> OpenApiRouter<Arc<ServerState>> {
    optionally_secured(
        OpenApiRouter::new().routes(routes!(caches::proto::cache_proto)),
        CACHE_BASIC,
    )
}

pub(crate) fn scim_routes() -> OpenApiRouter<Arc<ServerState>> {
    secured(
        OpenApiRouter::new()
            .routes(routes!(scim::discovery::service_provider_config))
            .routes(routes!(scim::discovery::resource_types))
            .routes(routes!(scim::discovery::schemas))
            .routes(routes!(scim::users::list, scim::users::create))
            .routes(routes!(
                scim::users::get,
                scim::users::replace,
                scim::users::patch,
                scim::users::delete
            ))
            .routes(routes!(scim::groups::list))
            .routes(routes!(scim::groups::get, scim::groups::patch)),
        SCIM,
    )
}

pub(crate) fn proto_api() -> OpenApiRouter<Arc<ServerState>> {
    unsecured(proto_router())
}
