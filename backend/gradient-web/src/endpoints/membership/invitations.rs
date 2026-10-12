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
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PendingInvitationItem {
    pub user: String,
    pub name: String,
    pub role: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

impl PendingInvitationItem {
    pub fn new(
        invitee_id: UserId,
        invitee: Option<&MUser>,
        role: String,
        created_at: NaiveDateTime,
        expires_at: NaiveDateTime,
    ) -> Self {
        Self {
            user: invitee
                .map(|u| u.username.clone())
                .unwrap_or_else(|| invitee_id.to_string()),
            name: invitee.map(|u| u.name.clone()).unwrap_or_default(),
            role,
            created_at,
            expires_at,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct RevokeInvitationRequest {
    pub user: String,
}

pub async fn find_invitation<T: InvitationTable>(
    db: &WebDb,
    owner: T::Id,
    user_id: UserId,
) -> WebResult<Option<T::Row>> {
    Ok(gradient_db::memberships::by_owner_and_user::<T, _>(
        db,
        T::owner_column(),
        owner,
        T::user_column(),
        user_id,
    )
    .await?)
}

pub async fn list<O: Owner>(
    scope: O::ListInvitationsScope,
) -> WebResult<Json<BaseResponse<Vec<PendingInvitationItem>>>> {
    let db = &scope.db().web_db;
    let rows = gradient_db::memberships::with_users::<O::Invitations, _>(
        db,
        <O::Invitations as InvitationTable>::user_relation(),
        <O::Invitations as InvitationTable>::owner_column(),
        scope.id(),
    )
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
            let role = role_map
                .get(&fields.role)
                .cloned()
                .unwrap_or_else(|| fields.role.to_string());
            PendingInvitationItem::new(
                fields.user,
                invitee.as_ref(),
                role,
                fields.created_at,
                fields.expires_at,
            )
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
