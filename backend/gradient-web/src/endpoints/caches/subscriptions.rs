/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::CacheRefusal;
use crate::access::{
    Caller, api_key_is_pinned_to_other_project, caller_cache_mask, project_admin_emails,
};
use crate::api_spec::CachePath;
use crate::audit::Audit;
use crate::endpoints::projects::settings::mode_label;
use crate::endpoints::projects::workers::reauth_project_workers;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use crate::scope::{CacheScope, Readable, Requires, can};
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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SubscriptionRequestItem {
    pub project: String,
    pub project_display_name: String,
    pub mode: CacheSubscriptionMode,
    pub requested_by: String,
    pub requested_by_display_name: Option<String>,
    pub created_at: NaiveDateTime,
}

impl SubscriptionRequestItem {
    fn new(
        request: &MCacheSubscriptionRequest,
        project: &MProject,
        requester: Option<&MUser>,
    ) -> Self {
        Self {
            project: project.name.clone(),
            project_display_name: project.display_name.clone(),
            mode: request.mode.clone(),
            requested_by: requester
                .map(|u| u.username.clone())
                .unwrap_or_else(|| request.requested_by.to_string()),
            requested_by_display_name: requester.map(|u| u.name.clone()),
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
    description = "Projects that asked to subscribe to this cache and await a decision. Requires `manageCacheSubscriptions`.",
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

    let requesters: HashMap<UserId, MUser> = by_ids::<EUser, _>(
        &db.web_db,
        &requests.iter().map(|r| r.requested_by).collect::<Vec<_>>(),
    )
    .await?;

    let items: Vec<SubscriptionRequestItem> = requests
        .iter()
        .filter_map(|r| {
            let project = projects.get(&r.project)?;
            Some(SubscriptionRequestItem::new(
                r,
                project,
                requesters.get(&r.requested_by),
            ))
        })
        .collect();

    Ok(ok_json(items))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ApproveSubscriptionRequest {
    pub mode: Option<CacheSubscriptionMode>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CacheSubscriberItem {
    pub project: String,
    pub project_display_name: String,
    pub mode: CacheSubscriptionMode,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CacheSubscribersResponse {
    pub projects: Vec<CacheSubscriberItem>,
    pub hidden: usize,
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/subscribers",
    operation_id = "listCacheSubscribers",
    tag = "caches",
    summary = "List of subscribed projects",
    description = "Projects that subscribe to this cache and their subscription mode. Follows the visibility of GET /caches/{cache}. The caller can view a project when the project is public, when the caller is a member, or through manageCacheSubscriptions on the cache. hidden is the number of other subscribed projects.",
    params(CachePath),
    responses((status = OK, description = "Subscribed projects that the caller can view. Ordered by display name.", body = CacheSubscribersResponse), WebError)
)]
pub async fn get_cache_subscribers(
    CacheScope {
        cache,
        user: maybe_user,
        db,
        api_key,
        ..
    }: CacheScope<Readable>,
) -> WebResult<Json<BaseResponse<CacheSubscribersResponse>>> {
    let subscriptions =
        gradient_db::projects::caches::subscriptions_to(&db.web_db, cache.id).await?;
    let project_ids: Vec<ProjectId> = subscriptions.iter().map(|s| s.project).collect();
    let projects: HashMap<ProjectId, MProject> =
        by_ids::<EProject, _>(&db.web_db, &project_ids).await?;

    let (sees_all, member_of) = match &maybe_user {
        Some(user) => {
            let manages = crate::permissions::cache_mask_grants(
                caller_cache_mask(&db.web_db, Caller::User(user), cache.id, api_key.as_ref())
                    .await?,
                crate::permissions::CachePermission::ManageCacheSubscriptions,
            );
            let mut member_of =
                gradient_db::access::project_role_names(&db.web_db, user.id, project_ids.clone())
                    .await?;
            member_of.retain(|project, _| {
                !api_key_is_pinned_to_other_project(api_key.as_ref(), *project)
            });
            (user.superuser || manages, member_of)
        }
        None => (false, HashMap::new()),
    };

    let mut visible: Vec<CacheSubscriberItem> = subscriptions
        .iter()
        .filter_map(|s| {
            let project = projects.get(&s.project)?;
            (sees_all || project.public || member_of.contains_key(&project.id)).then(|| {
                CacheSubscriberItem {
                    project: project.name.clone(),
                    project_display_name: project.display_name.clone(),
                    mode: s.mode.clone(),
                }
            })
        })
        .collect();
    visible.sort_by(|a, b| a.project_display_name.cmp(&b.project_display_name));
    let hidden = projects.len() - visible.len();

    Ok(ok_json(CacheSubscribersResponse {
        projects: visible,
        hidden,
    }))
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
    description = "Convert pending requests into actual subscriptions. Requeue evaluations with no cache. Subscriptions take the mode from the project request. The optional body `mode` can lower access, for example from read-write to read-only. The body cannot grant more access. Provide `manageCacheSubscriptions`. The endpoint can also work with caches from the server configuration.",
    params(CachePath, ("project" = String, Path)),
    request_body = Option<ApproveSubscriptionRequest>,
    responses((status = OK, description = "Subscription approved", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_approve_subscription_request(
    CacheScope {
        cache,
        user,
        db,
        params,
        ..
    }: CacheScope<Requires<can::ManageCacheSubscriptions>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    body: Option<Json<Option<ApproveSubscriptionRequest>>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project: String = params.param("project")?;

    let (project, request) = load_request(&db, cache.id, project).await?;
    let mode = match body.and_then(|Json(b)| b?.mode) {
        Some(mode) if !mode.grants_no_access_beyond(&request.mode) => {
            return Err(CacheRefusal::GrantedModeHasMoreAccessThanRequested.into());
        }
        Some(mode) => mode,
        None => request.mode.clone(),
    };

    let tx = state.web_db.inner().begin().await?;
    if !gradient_db::rows::delete_was_present::<ECacheSubscriptionRequest, _>(&tx, request).await? {
        return Err(WebError::not_found(Resource::SubscriptionRequest));
    }
    gradient_db::rows::insert::<EProjectCache, _>(
        &tx,
        MProjectCache {
            project: project.id,
            cache: cache.id,
            mode: mode.clone(),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::CacheSubscription))?;
    tx.commit().await?;

    reauth_project_workers(&state.web_db, &scheduler, project.id).await;

    if mode.writes_build_outputs_to_cache()
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
    }: CacheScope<Requires<can::ManageCacheSubscriptions>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::rows;
    use gradient_fixtures::test_date;

    fn request() -> MCacheSubscriptionRequest {
        MCacheSubscriptionRequest {
            id: CacheSubscriptionRequestId::now_v7(),
            project: rows::project().id,
            cache: CacheId::now_v7(),
            mode: CacheSubscriptionMode::ReadWrite,
            requested_by: rows::user().id,
            created_at: test_date(),
        }
    }

    #[test]
    fn a_request_names_the_requester_by_username_and_display_name() {
        let item = SubscriptionRequestItem::new(&request(), &rows::project(), Some(&rows::user()));
        assert_eq!(item.requested_by, "testuser");
        assert_eq!(item.requested_by_display_name.as_deref(), Some("Test User"));
    }

    #[test]
    fn a_request_from_a_deleted_user_falls_back_to_the_user_id() {
        let request = request();
        let item = SubscriptionRequestItem::new(&request, &rows::project(), None);
        assert_eq!(item.requested_by, request.requested_by.to_string());
        assert_eq!(item.requested_by_display_name, None);
    }
}
