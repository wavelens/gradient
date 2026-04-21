/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{load_editable_org, load_org_member};
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::db::get_any_cache_by_name;
use core::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_WRITE_ID};
use core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct SubscribeCacheRequest {
    pub mode: Option<CacheSubscriptionMode>,
}

#[derive(Serialize)]
pub struct CacheSubscriptionItem {
    pub id: Uuid,
    pub name: String,
    pub mode: CacheSubscriptionMode,
}

// ── Access helpers ────────────────────────────────────────────────────────────

/// Verify that `user_id` has Write or Admin role in `org_id`.
async fn require_write_permission(
    state: &Arc<ServerState>,
    org_id: Uuid,
    user_id: Uuid,
) -> WebResult<()> {
    let org_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(org_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.db)
        .await?;

    let has_write = matches!(
        org_user,
        Some(ref ou) if ou.role == BASE_ROLE_ADMIN_ID || ou.role == BASE_ROLE_WRITE_ID
    );

    if !has_write {
        return Err(WebError::Forbidden(
            "You need Write or Admin permissions in this organization to manage cache subscriptions"
                .to_string(),
        ));
    }

    Ok(())
}

/// Load a public or owned cache by name; verify the requesting user may subscribe to it.
async fn load_subscribable_cache(
    state: &Arc<ServerState>,
    cache_name: String,
    user_id: Uuid,
) -> WebResult<MCache> {
    let cache = get_any_cache_by_name(Arc::clone(state), cache_name)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public && cache.created_by != user_id {
        return Err(WebError::Forbidden(
            "You don't have permission to subscribe to this cache. The cache is private and you are not the owner.".to_string(),
        ));
    }

    Ok(cache)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn post_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_editable_org(&state, user.id, organization).await?;
    let mut active: AOrganization = org.into();
    active.public = Set(true);
    active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Organization is now public".to_string(),
    }))
}

pub async fn delete_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_editable_org(&state, user.id, organization).await?;
    let mut active: AOrganization = org.into();
    active.public = Set(false);
    active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Organization is now private".to_string(),
    }))
}

pub async fn get_organization_subscribe(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<CacheSubscriptionItem>>>> {
    let org = load_org_member(&state, user.id, organization).await?;

    let org_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org.id))
        .all(&state.db)
        .await?;

    let mut subscribed = Vec::new();
    for oc in org_caches {
        if let Ok(Some(cache)) = ECache::find_by_id(oc.cache).one(&state.db).await {
            subscribed.push(CacheSubscriptionItem {
                id: oc.cache,
                name: cache.name,
                mode: oc.mode,
            });
        }
    }

    Ok(Json(BaseResponse {
        error: false,
        message: subscribed,
    }))
}

pub async fn post_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, cache)): Path<(String, String)>,
    body: Option<Json<SubscribeCacheRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org_member(&state, user.id, organization).await?;
    require_write_permission(&state, org.id, user.id).await?;

    let cache = load_subscribable_cache(&state, cache, user.id).await?;

    let already = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(org.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await?;

    if already.is_some() {
        return Err(WebError::already_exists(
            "Organization already subscribed to Cache",
        ));
    }

    let mode = body
        .and_then(|b| b.mode.clone())
        .unwrap_or(CacheSubscriptionMode::ReadWrite);

    AOrganizationCache {
        id: Set(Uuid::new_v4()),
        organization: Set(org.id),
        cache: Set(cache.id),
        mode: Set(mode),
    }
    .insert(&state.db)
    .await?;

    // Enqueue signing of every cached path the org already owns for this
    // new cache. We insert `cached_path_signature` placeholders with
    // `signature = NULL`; the periodic sign sweep will fill them in.
    enqueue_backfill_signatures(&state, org.id, cache.id).await;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache subscribed".to_string(),
    }))
}

/// Insert null-signature placeholders for every `cached_path` reachable
/// from a derivation owned by `org_id`, for `cache_id`. Idempotent —
/// existing rows are skipped. Best-effort: errors are logged, not
/// propagated.
async fn enqueue_backfill_signatures(state: &ServerState, org_id: Uuid, cache_id: Uuid) {
    let drv_ids: Vec<Uuid> = match EDerivation::find()
        .filter(CDerivation::Organization.eq(org_id))
        .all(&state.db)
        .await
    {
        Ok(rows) => rows.into_iter().map(|d| d.id).collect(),
        Err(e) => {
            tracing::warn!(%org_id, error = %e, "backfill: failed to load derivations");
            return;
        }
    };

    if drv_ids.is_empty() {
        return;
    }

    let outputs = match EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(drv_ids))
        .filter(CDerivationOutput::CachedPath.is_not_null())
        .all(&state.db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%org_id, error = %e, "backfill: failed to load derivation_outputs");
            return;
        }
    };

    let cp_ids: std::collections::HashSet<Uuid> =
        outputs.into_iter().filter_map(|o| o.cached_path).collect();

    let now = chrono::Utc::now().naive_utc();
    for cp_id in cp_ids {
        let exists = ECachedPathSignature::find()
            .filter(CCachedPathSignature::CachedPath.eq(cp_id))
            .filter(CCachedPathSignature::Cache.eq(cache_id))
            .one(&state.db)
            .await
            .unwrap_or(None)
            .is_some();
        if exists {
            continue;
        }
        let am = ACachedPathSignature {
            id: Set(Uuid::new_v4()),
            cached_path: Set(cp_id),
            cache: Set(cache_id),
            signature: Set(None),
            created_at: Set(now),
        };
        if let Err(e) = am.insert(&state.db).await {
            tracing::warn!(cached_path = %cp_id, cache = %cache_id, error = %e, "backfill: placeholder insert failed");
        }
    }
}

pub async fn delete_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, cache)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org_member(&state, user.id, organization).await?;
    require_write_permission(&state, org.id, user.id).await?;

    let cache = load_subscribable_cache(&state, cache, user.id).await?;

    let record = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(org.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("Organization not subscribed to Cache".to_string()))?;

    let active: AOrganizationCache = record.into();
    active.delete(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache unsubscribed".to_string(),
    }))
}
