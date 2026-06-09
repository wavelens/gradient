/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::CachePermission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, PaginatorTrait, QueryFilter,
    QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMemberItem {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AddCacheMemberRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveCacheMemberRequest {
    pub user: String,
}

async fn find_user_by_username(state: &Arc<ServerState>, username: &str) -> WebResult<MUser> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(&state.web_db)
        .await?
        .or_not_found("User")
}

async fn find_cache_membership(
    state: &Arc<ServerState>,
    cache_id: CacheId,
    user_id: UserId,
) -> WebResult<Option<MCacheUser>> {
    Ok(ECacheUser::find()
        .filter(
            Condition::all()
                .add(CCacheUser::Cache.eq(cache_id))
                .add(CCacheUser::User.eq(user_id)),
        )
        .one(&state.web_db)
        .await?)
}

pub async fn get_cache_members(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<CacheMemberItem>>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ViewCache,
            reject_managed: false,
        },
    )
    .await?;

    let cache_users = ECacheUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::cache_user::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CCacheUser::Cache.eq(cache.id))
        .all(&state.web_db)
        .await?;

    let role_ids: Vec<RoleId> = cache_users.iter().map(|(cu, _)| cu.role).collect();
    let role_map: std::collections::HashMap<RoleId, String> = ECacheRole::find()
        .filter(CCacheRole::Id.is_in(role_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    let items: Vec<CacheMemberItem> = cache_users
        .iter()
        .map(|(cu, user)| CacheMemberItem {
            id: user
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| cu.user.to_string()),
            name: role_map
                .get(&cu.role)
                .cloned()
                .unwrap_or_else(|| cu.role.to_string()),
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn post_cache_member(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<AddCacheMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    if find_cache_membership(&state, cache.id, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists("User already in Cache"));
    }

    let role = ECacheRole::find()
        .filter(
            Condition::all()
                .add(CCacheRole::Name.eq(body.role.clone()))
                .add(
                    Condition::any()
                        .add(CCacheRole::Cache.eq(cache.id))
                        .add(CCacheRole::Cache.is_null()),
                ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    ACacheUser {
        id: Set(CacheUserId::now_v7()),
        cache: Set(cache.id),
        user: Set(target_user.id),
        role: Set(role.id),
    }
    .insert(&state.web_db)
    .await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_MEMBER_CREATE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "target_user_id": target_user.id.to_string(),
            "role": role.name,
        })),
    )
    .await;

    Ok(ok_json("User added".to_string()))
}

pub async fn patch_cache_member(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<AddCacheMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    let membership = find_cache_membership(&state, cache.id, target_user.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Cache"))?;

    let previous_role_id = membership.role;
    let role = ECacheRole::find()
        .filter(
            Condition::all()
                .add(CCacheRole::Name.eq(body.role.clone()))
                .add(
                    Condition::any()
                        .add(CCacheRole::Cache.eq(cache.id))
                        .add(CCacheRole::Cache.is_null()),
                ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    let mut active: ACacheUser = membership.into();
    active.role = Set(role.id);
    active.update(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_MEMBER_UPDATE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "target_user_id": target_user.id.to_string(),
            "previous_role_id": previous_role_id.to_string(),
            "new_role": role.name,
        })),
    )
    .await;

    Ok(ok_json("User role updated".to_string()))
}

pub async fn delete_cache_member(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<RemoveCacheMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    let membership = find_cache_membership(&state, cache.id, target_user.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Cache"))?;

    if membership.role == BASE_CACHE_ROLE_ADMIN_ID {
        let admin_count = ECacheUser::find()
            .filter(CCacheUser::Cache.eq(cache.id))
            .filter(CCacheUser::Role.eq(BASE_CACHE_ROLE_ADMIN_ID))
            .count(&state.web_db)
            .await?;
        if admin_count <= 1 {
            return Err(WebError::conflict(
                "Cannot remove the last Admin from the cache.",
            ));
        }
    }

    let active: ACacheUser = membership.into();
    active.delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_MEMBER_DELETE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "target_user_id": target_user.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("User removed".to_string()))
}
