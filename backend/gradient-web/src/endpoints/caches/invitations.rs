/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::Audit;
use crate::error::WebResult;
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{CacheScope, Requires, RequiresUnmanaged, can};
use axum::Json;
use chrono::NaiveDateTime;
use gradient_db::lookup::by_ids;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};

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
    CacheScope { cache, db, .. }: CacheScope<Requires<can::ManageCacheMembers>>,
) -> WebResult<Json<BaseResponse<Vec<PendingCacheInvitationItem>>>> {
    let rows = ECacheInvitation::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::cache_invitation::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CCacheInvitation::Cache.eq(cache.id))
        .all(&db.web_db)
        .await?;

    let role_ids: Vec<RoleId> = rows.iter().map(|(inv, _)| inv.role).collect();
    let role_map: std::collections::HashMap<RoleId, String> =
        by_ids::<ECacheRole, _>(&db.web_db, &role_ids)
            .await?
            .into_iter()
            .map(|(id, r)| (id, r.name))
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
    CacheScope { cache, db, .. }: CacheScope<RequiresUnmanaged<can::ManageCacheMembers>>,
    audit: Audit,
    Json(body): Json<RevokeCacheInvitationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::User)?;

    let invitation = ECacheInvitation::find()
        .filter(
            Condition::all()
                .add(CCacheInvitation::Cache.eq(cache.id))
                .add(CCacheInvitation::User.eq(target_user.id)),
        )
        .one(&db.web_db)
        .await?
        .ok_or(SharedRefusal::NoPendingInvitation)?;

    invitation.into_active_model().delete(&db.web_db).await?;

    audit.write(Action::CacheInvitationRevoke, EventOwner::cache(cache.id), serde_json::json!({ "cache_id": cache.id.to_string(), "target_user_id": target_user.id.to_string(), }))
    .await;

    Ok(ok_json("Invitation revoked".to_string()))
}
