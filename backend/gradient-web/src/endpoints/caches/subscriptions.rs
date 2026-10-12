/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::project_admin_emails;
use crate::api_spec::CachePath;
use crate::audit::Audit;
use crate::endpoints::projects::settings::mode_label;
use crate::endpoints::projects::workers::reauth_project_workers;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use crate::scope::{CacheScope, Requires, RequiresUnmanaged, can};
use axum::extract::State;
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_ci::CiContext;
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_db::lookup::by_ids;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_notify::{SubscriptionEvent, SubscriptionMail};
use gradient_scheduler::Scheduler;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::TransactionTrait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SubscriptionRequestItem {
    pub project: String,
    pub project_display_name: String,
    pub mode: CacheSubscriptionMode,
    pub requested_by: String,
    pub created_at: NaiveDateTime,
}

impl SubscriptionRequestItem {
    fn new(request: &MCacheSubscriptionRequest, project: &MProject, requested_by: String) -> Self {
        Self {
            project: project.name.clone(),
            project_display_name: project.display_name.clone(),
            mode: request.mode.clone(),
            requested_by,
            created_at: request.created_at,
        }
    }
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/subscription-requests",
    operation_id = "listCacheSubscriptionRequests",
    tag = "caches",
    summary = "List subscription requests",
    description = "Projects that have asked to subscribe to this cache and are waiting for\na decision. Requires `manageCacheSubscriptions`.",
    params(CachePath),
    responses((status = OK, description = "Pending requests, newest first", body = Vec<SubscriptionRequestItem>), WebError)
)]
pub async fn get_cache_subscription_requests(
    CacheScope { cache, db, .. }: CacheScope<Requires<can::ManageCacheSubscriptions>>,
) -> WebResult<Json<BaseResponse<Vec<SubscriptionRequestItem>>>> {
    let mut requests =
        gradient_db::cache_subscription_requests::of_cache(&db.web_db, cache.id).await?;
    requests.sort_by_key(|r| std::cmp::Reverse(r.created_at));

    let projects: HashMap<ProjectId, MProject> = by_ids::<EProject, _>(
        &db.web_db,
        &requests.iter().map(|r| r.project).collect::<Vec<_>>(),
    )
    .await?;

    let requesters: HashMap<UserId, String> = by_ids::<EUser, _>(
        &db.web_db,
        &requests.iter().map(|r| r.requested_by).collect::<Vec<_>>(),
    )
    .await?
    .into_iter()
    .map(|(id, u)| (id, u.username))
    .collect();

    let items: Vec<SubscriptionRequestItem> = requests
        .iter()
        .filter_map(|r| {
            let project = projects.get(&r.project)?;
            let requested_by = requesters
                .get(&r.requested_by)
                .cloned()
                .unwrap_or_else(|| r.requested_by.to_string());
            Some(SubscriptionRequestItem::new(r, project, requested_by))
        })
        .collect();

    Ok(ok_json(items))
}

async fn load_request(
    db: &DbContext,
    cache_id: CacheId,
    project_name: String,
) -> WebResult<(MProject, MCacheSubscriptionRequest)> {
    let project = gradient_db::lookup::get_any_project_by_name(db, project_name)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::SubscriptionRequest))?;

    let request = gradient_db::cache_subscription_requests::by_project_and_cache(
        &db.web_db, project.id, cache_id,
    )
    .await?
    .ok_or_else(|| WebError::not_found(Resource::SubscriptionRequest))?;

    Ok((project, request))
}

async fn notify_project_admins(
    ci: &CiContext,
    project: &MProject,
    cache: &MCache,
    actor: &MUser,
    mode: &CacheSubscriptionMode,
    event: SubscriptionEvent,
) {
    if !ci.email.is_enabled() {
        return;
    }

    let recipients = match project_admin_emails(&ci.db.web_db, project.id).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(error = %e, "failed to resolve project admin recipients");
            return;
        }
    };

    if let Err(e) = ci
        .email
        .send_subscription_mail(
            &recipients,
            &SubscriptionMail {
                event,
                project_display_name: &project.display_name,
                cache_display_name: &cache.display_name,
                mode: mode_label(mode),
                actor: &actor.username,
                link: format!(
                    "{}/project/{}/caches",
                    ci.db.config.server.serve_url, project.name
                ),
            },
        )
        .await
    {
        tracing::warn!(error = %e, "failed to send cache subscription decision email");
    }
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/subscription-requests/{project}",
    operation_id = "approveCacheSubscriptionRequest",
    tag = "caches",
    summary = "Approve a subscription request",
    description = "Turns the pending request into a real subscription, using the mode the\nproject asked for, and re-queues any evaluation parked for want of a\ncache. Requires `manageCacheSubscriptions`.",
    params(CachePath, ("project" = String, Path)),
    responses((status = OK, description = "Subscription approved", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_approve_subscription_request(
    CacheScope {
        cache,
        user,
        db,
        params,
        ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheSubscriptions>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project: String = params.param("project")?;

    let (project, request) = load_request(&db, cache.id, project).await?;
    let mode = request.mode.clone();

    let tx = state.web_db.inner().begin().await?;
    if !gradient_db::rows::delete_was_present::<ECacheSubscriptionRequest, _>(&tx, request).await? {
        return Err(WebError::not_found(Resource::SubscriptionRequest));
    }
    gradient_db::rows::insert::<EProjectCache, _>(
        &tx,
        MProjectCache {
            id: ProjectCacheId::now_v7(),
            project: project.id,
            cache: cache.id,
            mode: mode.clone(),
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::CacheSubscription))?;
    tx.commit().await?;

    reauth_project_workers(&state.web_db, &scheduler, project.id).await;

    let unparks_builds = matches!(
        mode,
        CacheSubscriptionMode::ReadWrite | CacheSubscriptionMode::WriteOnly
    );

    if unparks_builds
        && let Err(e) = gradient_ci::unpark_no_cache_for_project(&state.web_db, project.id).await
    {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-cache evaluations after cache subscription",
        );
    }

    audit.write(Action::CacheSubscriptionApprove, EventOwner::project_cache(project.id, cache.id), serde_json::json!({ "cache_id": cache.id.to_string(), "project_id": project.id.to_string(), }))
    .await;

    notify_project_admins(
        &state.ci(),
        &project,
        &cache,
        &user,
        &mode,
        SubscriptionEvent::Approved,
    )
    .await;

    Ok(ok_json("Subscription approved".to_string()))
}

#[utoipa::path(
    delete,
    path = "/caches/{cache}/subscription-requests/{project}",
    operation_id = "denyCacheSubscriptionRequest",
    tag = "caches",
    summary = "Deny a subscription request",
    description = "Requires `manageCacheSubscriptions`.",
    params(CachePath, ("project" = String, Path)),
    responses((status = OK, description = "Subscription request denied", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_subscription_request(
    CacheScope {
        cache,
        user,
        db,
        params,
        ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheSubscriptions>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    let project: String = params.param("project")?;

    let (project, request) = load_request(&db, cache.id, project).await?;
    let mode = request.mode.clone();
    gradient_db::rows::delete::<ECacheSubscriptionRequest, _>(&state.web_db, request).await?;

    audit.write(Action::CacheSubscriptionDeny, EventOwner::project_cache(project.id, cache.id), serde_json::json!({ "cache_id": cache.id.to_string(), "project_id": project.id.to_string(), }))
    .await;

    notify_project_admins(
        &state.ci(),
        &project,
        &cache,
        &user,
        &mode,
        SubscriptionEvent::Denied,
    )
    .await;

    Ok(ok_json("Subscription request denied".to_string()))
}
