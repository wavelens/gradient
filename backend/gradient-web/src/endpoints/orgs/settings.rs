/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, OrgAccess, load_cache, load_org};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_entity::organization_cache::CacheSubscriptionMode;
use gradient_db::permissions::CachePermission;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SubscribeCacheRequest {
    pub mode: Option<CacheSubscriptionMode>,
}

#[derive(Serialize)]
pub struct CacheSubscriptionItem {
    pub id: CacheId,
    pub name: String,
    pub mode: CacheSubscriptionMode,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn post_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageOrgSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut active: AOrganization = org.into();
    active.public = Set(true);
    active.update(&state.web_db).await?;

    Ok(ok_json("Organization is now public".to_string()))
}

pub async fn delete_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageOrgSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut active: AOrganization = org.into();
    active.public = Set(false);
    active.update(&state.web_db).await?;

    Ok(ok_json("Organization is now private".to_string()))
}

pub async fn get_organization_subscribe(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<CacheSubscriptionItem>>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let org_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org.id))
        .all(&state.web_db)
        .await?;

    let mut subscribed = Vec::new();
    for oc in org_caches {
        if let Ok(Some(cache)) = ECache::find_by_id(oc.cache).one(&state.web_db).await {
            subscribed.push(CacheSubscriptionItem {
                id: oc.cache,
                name: cache.name,
                mode: oc.mode,
            });
        }
    }

    Ok(ok_json(subscribed))
}

pub async fn post_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, cache)): Path<(String, String)>,
    body: Option<Json<SubscribeCacheRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
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
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSubscriptions,
            reject_managed: false,
        },
    )
    .await?;

    let already = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(org.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await?;

    if already.is_some() {
        return Err(WebError::already_exists(
            "Organization already subscribed to Cache",
        ));
    }

    let mode = body
        .and_then(|b| b.mode.clone())
        .unwrap_or(CacheSubscriptionMode::ReadWrite);

    let unparks_builds = matches!(
        mode,
        CacheSubscriptionMode::ReadWrite | CacheSubscriptionMode::WriteOnly
    );

    MOrganizationCache {
        id: OrganizationCacheId::now_v7(),
        organization: org.id,
        cache: cache.id,
        mode,
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    // Re-queue any evaluations parked with WaitingReason::NoCache for this
    // org. Only ReadWrite/WriteOnly subscriptions unblock builds; ReadOnly
    // subscriptions leave the org without anywhere to push outputs.
    if unparks_builds
        && let Err(e) = gradient_ci::unpark_no_cache_for_org(&state.web_db, org.id).await
    {
        tracing::warn!(
            error = %e,
            org_id = %org.id,
            "failed to unpark no-cache evaluations after cache subscription",
        );
    }

    // Enqueue signing of every cached path the org already owns for this
    // new cache. We insert `cached_path_signature` placeholders with
    // `signature = NULL`; the periodic sign sweep will fill them in.
    enqueue_backfill_signatures(&state, org.id, cache.id).await;

    Ok(ok_json("Cache subscribed".to_string()))
}

/// Insert null-signature placeholders for every `cached_path` reachable
/// from a derivation owned by `org_id`, for `cache_id`. Idempotent -
/// existing rows are skipped. Best-effort: errors are logged, not
/// propagated.
async fn enqueue_backfill_signatures(
    state: &ServerState,
    org_id: OrganizationId,
    cache_id: CacheId,
) {
    let drv_ids: Vec<DerivationId> = match EDerivation::find()
        .filter(CDerivation::Organization.eq(org_id))
        .all(&state.web_db)
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
        .all(&state.web_db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%org_id, error = %e, "backfill: failed to load derivation_outputs");
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

pub async fn delete_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, cache)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
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
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSubscriptions,
            reject_managed: false,
        },
    )
    .await?;

    let record = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(org.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::bad_request("Organization not subscribed to Cache"))?;

    let active: AOrganizationCache = record.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json("Cache unsubscribed".to_string()))
}
