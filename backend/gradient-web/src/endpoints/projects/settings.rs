/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::ProjectRefusal;
use crate::access::{CacheAccess, Caller, cache_admin_emails, has_cache_permission, load_cache};
use crate::api_spec::ProjectPath;
use crate::endpoints::projects::workers::reauth_project_workers;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Requires, RequiresUnmanaged, can};
use axum::extract::State;
use axum::{Extension, Json};
use gradient_ci::CiContext;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::lookup::by_ids;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_notify::{SubscriptionEvent, SubscriptionMail};
use gradient_scheduler::Scheduler;
use gradient_types::permissions::CachePermission;
use gradient_types::*;
use sea_orm::IntoActiveModel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

const SIGNATURE_INSERT_CHUNK: usize = 1_000;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SubscribeCacheRequest {
    pub mode: Option<CacheSubscriptionMode>,
}

#[derive(Clone, Copy, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionStatus {
    Active,
    Pending,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CacheSubscriptionItem {
    pub id: CacheId,
    pub name: String,
    pub mode: CacheSubscriptionMode,
    pub status: SubscriptionStatus,
}

#[utoipa::path(
    post,
    path = "/projects/{project}/public",
    operation_id = "makeProjectPublic",
    tag = "projects",
    summary = "Make project public",
    description = "Makes the project, its tasks, evaluations and builds readable without authentication. Requires the manage-project-settings permission; rejects state-managed projects.",
    params(ProjectPath),
    responses((status = OK, description = "Project is now public", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_project_public(
    ProjectScope { project, db, .. }: ProjectScope<RequiresUnmanaged<can::ManageProjectSettings>>,
) -> WebResult<Json<BaseResponse<String>>> {
    gradient_db::projects::set_public(&db.web_db, project, true).await?;

    Ok(ok_json("Project is now public".to_string()))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/public",
    operation_id = "makeProjectPrivate",
    tag = "projects",
    summary = "Make project private",
    description = "Restricts the project to its members. Requires the manage-project-settings permission; rejects state-managed projects.",
    params(ProjectPath),
    responses((status = OK, description = "Project is now private", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_project_public(
    ProjectScope { project, db, .. }: ProjectScope<RequiresUnmanaged<can::ManageProjectSettings>>,
) -> WebResult<Json<BaseResponse<String>>> {
    gradient_db::projects::set_public(&db.web_db, project, false).await?;

    Ok(ok_json("Project is now private".to_string()))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/subscribe",
    operation_id = "listProjectSubscriptions",
    tag = "projects",
    summary = "List cache subscriptions",
    description = "Returns the binary caches this project currently uses as substituters.",
    params(ProjectPath),
    responses((status = OK, description = "Subscribed caches", body = Vec<CacheSubscriptionItem>), WebError)
)]
pub async fn get_project_subscribe(
    ProjectScope { project, db, .. }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<CacheSubscriptionItem>>>> {
    let project_caches =
        gradient_db::projects::caches::subscriptions_of(&db.web_db, project.id).await?;

    let pending =
        gradient_db::cache_subscription_requests::of_project(&db.web_db, project.id).await?;

    let cache_ids: Vec<CacheId> = project_caches
        .iter()
        .map(|oc| oc.cache)
        .chain(pending.iter().map(|r| r.cache))
        .collect();
    let names: HashMap<CacheId, String> = by_ids::<ECache, _>(&db.web_db, &cache_ids)
        .await?
        .into_iter()
        .map(|(id, c)| (id, c.name))
        .collect();

    let active = project_caches.into_iter().map(|subscription| {
        (
            subscription.cache,
            subscription.mode,
            SubscriptionStatus::Active,
        )
    });
    let requested = pending
        .into_iter()
        .map(|request| (request.cache, request.mode, SubscriptionStatus::Pending));
    let subscribed: Vec<CacheSubscriptionItem> = active
        .chain(requested)
        .filter_map(|(id, mode, status)| {
            Some(CacheSubscriptionItem {
                id,
                name: names.get(&id)?.clone(),
                mode,
                status,
            })
        })
        .collect();

    Ok(ok_json(subscribed))
}

#[utoipa::path(
    post,
    path = "/projects/{project}/subscribe/{cache}",
    operation_id = "subscribeProjectToCache",
    tag = "projects",
    summary = "Subscribe to cache, or request a subscription",
    description = "Requires `manageSubscriptions` on the project, and the cache must be\nvisible to the caller.\n\nWhen the caller also holds `manageCacheSubscriptions` on the cache, or is\na superuser, the cache is added to the project's substituters\nimmediately. Otherwise a **subscription request** is recorded instead and\nthe response message is `Subscription requested`; a cache admin then\napproves it through\n`POST /caches/{cache}/subscription-requests/{project}`.",
    params(ProjectPath, ("cache" = String, Path)),
    request_body = Option<SubscribeCacheRequest>,
    responses((status = OK, description = "Subscribed, or subscription requested", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_project_subscribe_cache(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageSubscriptions>>,
    state: State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    body: Option<Json<SubscribeCacheRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache: String = params.param("cache")?;

    let cache = load_cache(
        &db,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

    let already =
        gradient_db::projects::caches::subscription(&state.web_db, project.id, cache.id).await?;

    if already.is_some() {
        return Err(WebError::already_exists(Resource::CacheSubscription));
    }

    let pending = gradient_db::cache_subscription_requests::by_project_and_cache(
        &state.web_db,
        project.id,
        cache.id,
    )
    .await?;

    if pending.is_some() {
        return Err(WebError::already_exists(Resource::SubscriptionRequest));
    }

    let mode = body
        .and_then(|b| b.mode.clone())
        .unwrap_or(CacheSubscriptionMode::ReadWrite);

    let may_approve = user.superuser
        || has_cache_permission(
            &state.web_db,
            user.id,
            cache.id,
            CachePermission::ManageCacheSubscriptions,
            api_key.as_ref(),
        )
        .await?;

    if !may_approve {
        gradient_db::rows::insert::<ECacheSubscriptionRequest, _>(
            &state.web_db,
            MCacheSubscriptionRequest {
                id: CacheSubscriptionRequestId::now_v7(),
                project: project.id,
                cache: cache.id,
                mode: mode.clone(),
                requested_by: user.id,
                created_at: gradient_types::now(),
            },
        )
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::SubscriptionRequest))?;

        notify_cache_admins(&state.ci(), &project, &cache, &user, &mode).await;

        return Ok(ok_json("Subscription requested".to_string()));
    }

    let unparks_builds = matches!(
        mode,
        CacheSubscriptionMode::ReadWrite | CacheSubscriptionMode::WriteOnly
    );

    gradient_db::rows::insert::<EProjectCache, _>(
        &state.web_db,
        MProjectCache {
            id: ProjectCacheId::now_v7(),
            project: project.id,
            cache: cache.id,
            mode,
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::CacheSubscription))?;

    reauth_project_workers(&state.web_db, &scheduler, project.id).await;

    // Re-queue the evaluations parked with `WaitingReason::NoCache`. Only ReadWrite and WriteOnly
    // subscriptions unblock builds. ReadOnly subscriptions leave the project without a push target.
    if unparks_builds
        && let Err(e) = gradient_ci::unpark_no_cache_for_project(&state.web_db, project.id).await
    {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-cache evaluations after cache subscription",
        );
    }

    enqueue_backfill_signatures(&state.web_db, project.id, cache.id).await;

    Ok(ok_json("Cache subscribed".to_string()))
}

async fn notify_cache_admins(
    ci: &CiContext,
    project: &MProject,
    cache: &MCache,
    requester: &MUser,
    mode: &CacheSubscriptionMode,
) {
    if !ci.email.is_enabled() {
        return;
    }

    let recipients = match cache_admin_emails(&ci.db.web_db, cache.id).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(error = %e, "failed to resolve cache admin recipients");
            return;
        }
    };

    if let Err(e) = ci
        .email
        .send_subscription_mail(
            &recipients,
            &SubscriptionMail {
                event: SubscriptionEvent::Requested,
                project_display_name: &project.display_name,
                cache_display_name: &cache.display_name,
                mode: mode_label(mode),
                actor: &requester.username,
                link: format!(
                    "{}/caches/{}/subscriptions",
                    ci.db.config.server.serve_url, cache.name
                ),
            },
        )
        .await
    {
        tracing::warn!(error = %e, "failed to send cache subscription request email");
    }
}

pub fn mode_label(mode: &CacheSubscriptionMode) -> &'static str {
    match mode {
        CacheSubscriptionMode::ReadWrite => "read and write",
        CacheSubscriptionMode::ReadOnly => "read only",
        CacheSubscriptionMode::WriteOnly => "write only",
    }
}

async fn enqueue_backfill_signatures(db: &WebDb, project_id: ProjectId, cache_id: CacheId) {
    let drv_ids = match gradient_db::projects::derivations::derivation_ids_for_project(
        db, project_id,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(%project_id, error = %e, "backfill: failed to load derivations");
            return;
        }
    };

    if drv_ids.is_empty() {
        return;
    }

    let outputs = match gradient_db::derivation_outputs::of_derivations(db, &drv_ids).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%project_id, error = %e, "backfill: failed to load derivation_outputs");
            return;
        }
    };
    let hashes: Vec<String> = outputs.into_iter().filter_map(|o| o.hash).collect();
    let stored = match gradient_db::cached_paths::stored_by_hash(db, &hashes).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%project_id, error = %e, "backfill: failed to load cached paths");
            return;
        }
    };
    let cp_ids: std::collections::HashSet<CachedPathId> = stored.into_values().collect();

    // The (cached_path, cache) uniqueness is doing the de-duplication. The whole backfill is one
    // batched insert instead of a check and an insert per path.
    let now = gradient_types::now();
    let rows: Vec<_> = cp_ids
        .into_iter()
        .map(|cp_id| {
            MCachedPathSignature {
                id: CachedPathSignatureId::now_v7(),
                cached_path: cp_id,
                cache: cache_id,
                created_at: now,
                ..Default::default()
            }
            .into_active_model()
        })
        .collect();

    for chunk in rows.chunks(SIGNATURE_INSERT_CHUNK) {
        let res = gradient_db::cached_paths::add_missing_signatures(db, chunk.to_vec()).await;
        if let Err(e) = res
            && !matches!(e, sea_orm::DbErr::RecordNotInserted)
        {
            tracing::warn!(cache = %cache_id, error = %e, "backfill: placeholder insert failed");
        }
    }
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/subscribe/{cache}",
    operation_id = "unsubscribeProjectFromCache",
    tag = "projects",
    summary = "Unsubscribe from cache, or cancel a pending request",
    description = "Removes a binary cache from the project's substituters. When no\nsubscription exists but a request is still pending, the request is\ncancelled instead. Requires `manageSubscriptions` on the project.",
    params(ProjectPath, ("cache" = String, Path)),
    responses((status = OK, description = "Unsubscribed, or subscription request cancelled", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_project_subscribe_cache(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageSubscriptions>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache: String = params.param("cache")?;

    let cache = load_cache(
        &db,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

    let subscription =
        gradient_db::projects::caches::subscription(&db.web_db, project.id, cache.id).await?;

    let Some(subscription) = subscription else {
        let request = gradient_db::cache_subscription_requests::by_project_and_cache(
            &db.web_db, project.id, cache.id,
        )
        .await?
        .ok_or(ProjectRefusal::NotSubscribedToCache)?;

        gradient_db::rows::delete::<ECacheSubscriptionRequest, _>(&db.web_db, request).await?;

        return Ok(ok_json("Subscription request cancelled".to_string()));
    };

    gradient_db::rows::delete::<EProjectCache, _>(&db.web_db, subscription).await?;

    Ok(ok_json("Cache unsubscribed".to_string()))
}
