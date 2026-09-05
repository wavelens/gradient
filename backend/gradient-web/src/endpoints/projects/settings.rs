/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{
    CacheAccess, Caller, ProjectAccess, cache_admin_emails, has_cache_permission, load_cache,
    load_project,
};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::permissions::CachePermission;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_notify::{SubscriptionEvent, SubscriptionMail};
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SubscribeCacheRequest {
    pub mode: Option<CacheSubscriptionMode>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionStatus {
    Active,
    Pending,
}

#[derive(Serialize)]
pub struct CacheSubscriptionItem {
    pub id: CacheId,
    pub name: String,
    pub mode: CacheSubscriptionMode,
    pub status: SubscriptionStatus,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn post_project_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageProjectSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut active: AProject = project.into();
    active.public = Set(true);
    active.update(&state.web_db).await?;

    Ok(ok_json("Project is now public".to_string()))
}

pub async fn delete_project_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageProjectSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut active: AProject = project.into();
    active.public = Set(false);
    active.update(&state.web_db).await?;

    Ok(ok_json("Project is now private".to_string()))
}

pub async fn get_project_subscribe(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<CacheSubscriptionItem>>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let project_caches = EProjectCache::find()
        .filter(CProjectCache::Project.eq(project.id))
        .all(&state.web_db)
        .await?;

    let mut subscribed = Vec::new();
    for oc in project_caches {
        if let Ok(Some(cache)) = ECache::find_by_id(oc.cache).one(&state.web_db).await {
            subscribed.push(CacheSubscriptionItem {
                id: oc.cache,
                name: cache.name,
                mode: oc.mode,
                status: SubscriptionStatus::Active,
            });
        }
    }

    let pending = ECacheSubscriptionRequest::find()
        .filter(CCacheSubscriptionRequest::Project.eq(project.id))
        .all(&state.web_db)
        .await?;

    for request in pending {
        if let Ok(Some(cache)) = ECache::find_by_id(request.cache).one(&state.web_db).await {
            subscribed.push(CacheSubscriptionItem {
                id: request.cache,
                name: cache.name,
                mode: request.mode,
                status: SubscriptionStatus::Pending,
            });
        }
    }

    Ok(ok_json(subscribed))
}

pub async fn post_project_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, cache)): Path<(String, String)>,
    body: Option<Json<SubscribeCacheRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageSubscriptions,
            reject_managed: false,
        },
    )
    .await?;

    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

    let already = EProjectCache::find()
        .filter(
            Condition::all()
                .add(CProjectCache::Project.eq(project.id))
                .add(CProjectCache::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await?;

    if already.is_some() {
        return Err(WebError::already_exists(
            "Project already subscribed to Cache",
        ));
    }

    let pending = ECacheSubscriptionRequest::find()
        .filter(
            Condition::all()
                .add(CCacheSubscriptionRequest::Project.eq(project.id))
                .add(CCacheSubscriptionRequest::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await?;

    if pending.is_some() {
        return Err(WebError::already_exists("Subscription already requested"));
    }

    let mode = body
        .and_then(|b| b.mode.clone())
        .unwrap_or(CacheSubscriptionMode::ReadWrite);

    let may_approve = user.superuser
        || has_cache_permission(
            &state,
            user.id,
            cache.id,
            CachePermission::ManageCacheSubscriptions,
            api_key.as_ref(),
        )
        .await?;

    if !may_approve {
        MCacheSubscriptionRequest {
            id: CacheSubscriptionRequestId::now_v7(),
            project: project.id,
            cache: cache.id,
            mode: mode.clone(),
            requested_by: user.id,
            created_at: gradient_types::now(),
        }
        .into_active_model()
        .insert(&state.web_db)
        .await?;

        notify_cache_admins(&state, &project, &cache, &user, &mode).await;

        return Ok(ok_json("Subscription requested".to_string()));
    }

    let unparks_builds = matches!(
        mode,
        CacheSubscriptionMode::ReadWrite | CacheSubscriptionMode::WriteOnly
    );

    MProjectCache {
        id: ProjectCacheId::now_v7(),
        project: project.id,
        cache: cache.id,
        mode,
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    // Re-queue any evaluations parked with WaitingReason::NoCache for this
    // project. Only ReadWrite/WriteOnly subscriptions unblock builds; ReadOnly
    // subscriptions leave the project without anywhere to push outputs.
    if unparks_builds
        && let Err(e) = gradient_ci::unpark_no_cache_for_project(&state.web_db, project.id).await
    {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-cache evaluations after cache subscription",
        );
    }

    // Enqueue signing of every cached path the project already owns for this
    // new cache. We insert `cached_path_signature` placeholders with
    // `signature = NULL`; the periodic sign sweep will fill them in.
    enqueue_backfill_signatures(&state, project.id, cache.id).await;

    Ok(ok_json("Cache subscribed".to_string()))
}

/// Best-effort notice to the cache's admins that a project wants in. Silent
/// when SMTP is not configured: the request row and the cache's requests page
/// carry the flow on their own.
async fn notify_cache_admins(
    state: &Arc<ServerState>,
    project: &MProject,
    cache: &MCache,
    requester: &MUser,
    mode: &CacheSubscriptionMode,
) {
    if !state.email.is_enabled() {
        return;
    }

    let recipients = match cache_admin_emails(state, cache.id).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(error = %e, "failed to resolve cache admin recipients");
            return;
        }
    };

    if let Err(e) = state
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
                    state.config.server.serve_url, cache.name
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

/// Insert null-signature placeholders for every `cached_path` reachable
/// from a derivation the project has built, for `cache_id`. Idempotent -
/// existing rows are skipped. Best-effort: errors are logged, not
/// propagated.
async fn enqueue_backfill_signatures(
    state: &ServerState,
    project_id: ProjectId,
    cache_id: CacheId,
) {
    let drv_ids = match gradient_db::derivation_ids_for_project(&state.web_db, project_id).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(%project_id, error = %e, "backfill: failed to load derivations");
            return;
        }
    };

    if drv_ids.is_empty() {
        return;
    }

    let outputs = match EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(drv_ids))
        .filter(CDerivationOutput::CachedPath.is_not_null())
        .all(&state.web_db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%project_id, error = %e, "backfill: failed to load derivation_outputs");
            return;
        }
    };

    let cp_ids: std::collections::HashSet<CachedPathId> =
        outputs.into_iter().filter_map(|o| o.cached_path).collect();

    let now = gradient_types::now();
    for cp_id in cp_ids {
        let exists = ECachedPathSignature::find()
            .filter(CCachedPathSignature::CachedPath.eq(cp_id))
            .filter(CCachedPathSignature::Cache.eq(cache_id))
            .one(&state.web_db)
            .await
            .unwrap_or(None)
            .is_some();
        if exists {
            continue;
        }
        let am = MCachedPathSignature {
            id: CachedPathSignatureId::now_v7(),
            cached_path: cp_id,
            cache: cache_id,
            created_at: now,
            ..Default::default()
        }
        .into_active_model();

        if let Err(e) = am.insert(&state.web_db).await {
            tracing::warn!(cached_path = %cp_id, cache = %cache_id, error = %e, "backfill: placeholder insert failed");
        }
    }
}

pub async fn delete_project_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, cache)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageSubscriptions,
            reject_managed: false,
        },
    )
    .await?;

    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

    let record = EProjectCache::find()
        .filter(
            Condition::all()
                .add(CProjectCache::Project.eq(project.id))
                .add(CProjectCache::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await?;

    let Some(record) = record else {
        let request = ECacheSubscriptionRequest::find()
            .filter(
                Condition::all()
                    .add(CCacheSubscriptionRequest::Project.eq(project.id))
                    .add(CCacheSubscriptionRequest::Cache.eq(cache.id)),
            )
            .one(&state.web_db)
            .await?
            .ok_or_else(|| WebError::bad_request("Project not subscribed to Cache"))?;

        request.into_active_model().delete(&state.web_db).await?;

        return Ok(ok_json("Subscription request cancelled".to_string()));
    };

    let active: AProjectCache = record.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json("Cache unsubscribed".to_string()))
}
