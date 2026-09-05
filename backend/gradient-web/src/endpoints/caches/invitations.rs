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
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCacheInvitationItem {
    pub user: String,
    pub name: String,
    pub role: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RevokeCacheInvitationRequest {
    pub user: String,
}

pub async fn get_cache_invitations(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<PendingCacheInvitationItem>>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheMembers,
            reject_managed: false,
        },
    )
    .await?;

    let rows = ECacheInvitation::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::cache_invitation::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CCacheInvitation::Cache.eq(cache.id))
        .all(&state.web_db)
        .await?;

    let role_ids: Vec<RoleId> = rows.iter().map(|(inv, _)| inv.role).collect();
    let role_map: std::collections::HashMap<RoleId, String> = ECacheRole::find()
        .filter(CCacheRole::Id.is_in(role_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    let items: Vec<PendingCacheInvitationItem> = rows
        .iter()
        .map(|(inv, invitee)| PendingCacheInvitationItem {
            user: invitee
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| inv.user.to_string()),
            name: invitee.as_ref().map(|u| u.name.clone()).unwrap_or_default(),
            role: role_map
                .get(&inv.role)
                .cloned()
                .unwrap_or_else(|| inv.role.to_string()),
            created_at: inv.created_at,
            expires_at: inv.expires_at,
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn delete_cache_invitation(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<RevokeCacheInvitationRequest>,
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

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.web_db)
        .await?
        .or_not_found("User")?;

    let invitation = ECacheInvitation::find()
        .filter(
            Condition::all()
                .add(CCacheInvitation::Cache.eq(cache.id))
                .add(CCacheInvitation::User.eq(target_user.id)),
        )
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::bad_request("No pending invitation for this user"))?;

    invitation.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_INVITATION_REVOKE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "target_user_id": target_user.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("Invitation revoked".to_string()))
}
