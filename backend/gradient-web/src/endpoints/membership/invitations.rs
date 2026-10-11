/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{InvitationTable, Owner, OwnerScope, delete_row, find_user_by_username, role_names};
use crate::audit::Audit;
use crate::error::WebResult;
use crate::helpers::ok_json;
use crate::refusal::SharedRefusal;
use axum::Json;
use chrono::NaiveDateTime;
use gradient_db::WebDb;
use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInvitationItem {
    pub user: String,
    pub name: String,
    pub role: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RevokeInvitationRequest {
    pub user: String,
}

pub async fn find_invitation<T: InvitationTable>(
    db: &WebDb,
    owner: T::Id,
    user_id: UserId,
) -> WebResult<Option<T::Row>> {
    Ok(T::find()
        .filter(
            Condition::all()
                .add(T::owner_column().eq(owner))
                .add(T::user_column().eq(user_id)),
        )
        .one(db)
        .await?)
}

pub async fn list<O: Owner>(
    scope: O::ListInvitationsScope,
) -> WebResult<Json<BaseResponse<Vec<PendingInvitationItem>>>> {
    let db = &scope.db().web_db;
    let rows = <O::Invitations as EntityTrait>::find()
        .join(
            JoinType::InnerJoin,
            <O::Invitations as InvitationTable>::user_relation(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(<O::Invitations as InvitationTable>::owner_column().eq(scope.id()))
        .all(db)
        .await?;

    let role_ids: Vec<RoleId> = rows
        .iter()
        .map(|(inv, _)| <O::Invitations as InvitationTable>::fields(inv).role)
        .collect();
    let role_map = role_names::<O::Roles>(db, role_ids).await?;

    let items: Vec<PendingInvitationItem> = rows
        .iter()
        .map(|(inv, invitee)| {
            let fields = <O::Invitations as InvitationTable>::fields(inv);
            PendingInvitationItem {
                user: invitee
                    .as_ref()
                    .map(|u| u.username.clone())
                    .unwrap_or_else(|| fields.user.to_string()),
                name: invitee.as_ref().map(|u| u.name.clone()).unwrap_or_default(),
                role: role_map
                    .get(&fields.role)
                    .cloned()
                    .unwrap_or_else(|| fields.role.to_string()),
                created_at: fields.created_at,
                expires_at: fields.expires_at,
            }
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn revoke<O: Owner>(
    scope: O::ManageMembersScope,
    audit: Audit,
    Json(body): Json<RevokeInvitationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let owner = scope.id();
    let db = &scope.db().web_db;
    let target_user = find_user_by_username(db, &body.user).await?;

    let invitation = find_invitation::<O::Invitations>(db, owner, target_user.id)
        .await?
        .ok_or(SharedRefusal::NoPendingInvitation)?;

    delete_row::<O::Invitations>(db, invitation).await?;

    audit.write(O::ACTIONS.invitation_revoke, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "target_user_id": target_user.id.to_string(), }))
    .await;

    Ok(ok_json("Invitation revoked".to_string()))
}
