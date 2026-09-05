/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, load_cache, project_admin_emails};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::endpoints::projects::settings::mode_label;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::CachePermission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_notify::{SubscriptionEvent, SubscriptionMail};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
    TransactionTrait,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionRequestItem {
    pub project: String,
    pub project_display_name: String,
    pub mode: CacheSubscriptionMode,
    pub requested_by: String,
    pub created_at: NaiveDateTime,
}

pub async fn get_cache_subscription_requests(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<SubscriptionRequestItem>>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSubscriptions,
            reject_managed: false,
        },
    )
    .await?;

    let mut requests = ECacheSubscriptionRequest::find()
        .filter(CCacheSubscriptionRequest::Cache.eq(cache.id))
        .all(&state.web_db)
        .await?;
    requests.sort_by_key(|r| std::cmp::Reverse(r.created_at));

    let projects: HashMap<ProjectId, MProject> = EProject::find()
        .filter(CProject::Id.is_in(requests.iter().map(|r| r.project).collect::<Vec<_>>()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();

    let requesters: HashMap<UserId, String> = EUser::find()
        .filter(CUser::Id.is_in(requests.iter().map(|r| r.requested_by).collect::<Vec<_>>()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|u| (u.id, u.username))
        .collect();

    let items: Vec<SubscriptionRequestItem> = requests
        .iter()
        .filter_map(|r| {
            let project = projects.get(&r.project)?;
            Some(SubscriptionRequestItem {
                project: project.name.clone(),
                project_display_name: project.display_name.clone(),
                mode: r.mode.clone(),
                requested_by: requesters
                    .get(&r.requested_by)
                    .cloned()
                    .unwrap_or_else(|| r.requested_by.to_string()),
                created_at: r.created_at,
            })
        })
        .collect();

    Ok(ok_json(items))
}

async fn load_request(
    state: &Arc<ServerState>,
    cache_id: CacheId,
    project_name: String,
) -> WebResult<(MProject, MCacheSubscriptionRequest)> {
    let project = gradient_db::get_any_project_by_name(&state.db(), project_name)
        .await?
        .ok_or_else(|| WebError::not_found("Subscription request"))?;

    let request = ECacheSubscriptionRequest::find()
        .filter(
            Condition::all()
                .add(CCacheSubscriptionRequest::Cache.eq(cache_id))
                .add(CCacheSubscriptionRequest::Project.eq(project.id)),
        )
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::not_found("Subscription request"))?;

    Ok((project, request))
}

async fn notify_project_admins(
    state: &Arc<ServerState>,
    project: &MProject,
    cache: &MCache,
    actor: &MUser,
    mode: &CacheSubscriptionMode,
    event: SubscriptionEvent,
) {
    if !state.email.is_enabled() {
        return;
    }

    let recipients = match project_admin_emails(state, project.id).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(error = %e, "failed to resolve project admin recipients");
            return;
        }
    };

    if let Err(e) = state
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
                    state.config.server.serve_url, project.name
                ),
            },
        )
        .await
    {
        tracing::warn!(error = %e, "failed to send cache subscription decision email");
    }
}

pub async fn post_approve_subscription_request(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSubscriptions,
            reject_managed: true,
        },
    )
    .await?;

    let (project, request) = load_request(&state, cache.id, project).await?;
    let mode = request.mode.clone();

    let tx = state.web_db.inner().begin().await?;
    request.into_active_model().delete(&tx).await?;
    MProjectCache {
        id: ProjectCacheId::now_v7(),
        project: project.id,
        cache: cache.id,
        mode: mode.clone(),
    }
    .into_active_model()
    .insert(&tx)
    .await?;
    tx.commit().await?;

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

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_SUBSCRIPTION_APPROVE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "project_id": project.id.to_string(),
        })),
    )
    .await;

    notify_project_admins(
        &state,
        &project,
        &cache,
        &user,
        &mode,
        SubscriptionEvent::Approved,
    )
    .await;

    Ok(ok_json("Subscription approved".to_string()))
}

pub async fn delete_subscription_request(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSubscriptions,
            reject_managed: true,
        },
    )
    .await?;

    let (project, request) = load_request(&state, cache.id, project).await?;
    let mode = request.mode.clone();
    request.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_SUBSCRIPTION_DENY,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "project_id": project.id.to_string(),
        })),
    )
    .await;

    notify_project_admins(
        &state,
        &project,
        &cache,
        &user,
        &mode,
        SubscriptionEvent::Denied,
    )
    .await;

    Ok(ok_json("Subscription request denied".to_string()))
}
