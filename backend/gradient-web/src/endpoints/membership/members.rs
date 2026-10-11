/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::invitations::find_invitation;
use super::roles::owned_or_builtin;
use super::{
    ActingScope, InvitationTable, MemberTable, NewInvitation, Owner, OwnerScope, RoleFields,
    RoleTable, delete_row, find_user_by_username, insert_row, role_names, update_row,
};
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::invite_policy::invitation_expiry;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_notify::{InvitationMail, generate_token};
use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, JoinType, QueryFilter, QuerySelect};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberItem {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AddMemberRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveMemberRequest {
    pub user: String,
}

async fn find_membership<T: MemberTable>(
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

async fn find_role<T: RoleTable>(
    db: &WebDb,
    owner: T::Id,
    name: &str,
) -> WebResult<RoleFields<T::Id>> {
    let role = T::find()
        .filter(
            Condition::all()
                .add(T::name_column().eq(name))
                .add(owned_or_builtin::<T>(owner)),
        )
        .one(db)
        .await?
        .or_not_found(Resource::Role)?;

    Ok(T::fields(&role))
}

async fn with_users<T: MemberTable>(
    db: &WebDb,
    owner: T::Id,
) -> WebResult<Vec<(T::Row, Option<MUser>)>> {
    Ok(T::find()
        .join(JoinType::InnerJoin, T::user_relation())
        .select_also(gradient_entity::user::Entity)
        .filter(T::owner_column().eq(owner))
        .all(db)
        .await?)
}

pub async fn list<O: Owner>(
    scope: O::ListMembersScope,
) -> WebResult<Json<BaseResponse<Vec<MemberItem>>>> {
    let db = &scope.db().web_db;
    let members = with_users::<O::Members>(db, scope.id()).await?;

    let role_ids: Vec<RoleId> = members
        .iter()
        .map(|(member, _)| <O::Members as MemberTable>::fields(member).role)
        .collect();
    let role_map = role_names::<O::Roles>(db, role_ids).await?;

    let items: Vec<MemberItem> = members
        .iter()
        .map(|(member, user)| {
            let fields = <O::Members as MemberTable>::fields(member);
            MemberItem {
                id: user
                    .as_ref()
                    .map(|u| u.username.clone())
                    .unwrap_or_else(|| fields.user.to_string()),
                name: role_map
                    .get(&fields.role)
                    .cloned()
                    .unwrap_or_else(|| fields.role.to_string()),
            }
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn add<O: Owner>(
    scope: O::ManageMembersScope,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Json(body): Json<AddMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let owner = scope.id();
    let user = scope.user();
    let target_user = find_user_by_username(&state.web_db, &body.user).await?;

    if find_membership::<O::Members>(&state.web_db, owner, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(O::MEMBERSHIP));
    }

    if find_invitation::<O::Invitations>(&state.web_db, owner, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::Invitation));
    }

    let role = find_role::<O::Roles>(&state.web_db, owner, &body.role).await?;

    if user.superuser {
        insert_row::<O::Members>(
            &state.web_db,
            <O::Members as MemberTable>::new(owner, target_user.id, role.id),
        )
        .await?;

        audit.write(O::ACTIONS.member_add, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "target_user_id": target_user.id.to_string(), "role": role.name, }))
        .await;

        return Ok(ok_json("User added".to_string()));
    }

    let now = gradient_types::now();
    let token = generate_token();

    insert_row::<O::Invitations>(
        &state.web_db,
        <O::Invitations as InvitationTable>::new(NewInvitation {
            owner,
            user: target_user.id,
            role: role.id,
            invited_by: user.id,
            token: token.clone(),
            created_at: now,
            expires_at: invitation_expiry(now),
        }),
    )
    .await?;

    if state.email.is_enabled()
        && let Err(e) = state
            .email
            .send_invitation_email(
                &target_user.email,
                &target_user.name,
                &InvitationMail {
                    scope: O::INVITE_SCOPE,
                    scope_display_name: scope.display_name(),
                    role: &role.name,
                    inviter: &user.username,
                    accept_url: format!(
                        "{}/settings/invites?token={}",
                        state.config.server.serve_url, token
                    ),
                },
            )
            .await
    {
        tracing::warn!(error = %e, "Failed to send {} invitation email", O::KIND);
    }

    audit.write(O::ACTIONS.invitation_create, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "target_user_id": target_user.id.to_string(), "role": role.name, }))
    .await;

    Ok(ok_json("Invitation sent".to_string()))
}

pub async fn change_role<O: Owner>(
    scope: O::ManageMembersScope,
    audit: Audit,
    Json(body): Json<AddMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let owner = scope.id();
    let db = &scope.db().web_db;
    let target_user = find_user_by_username(db, &body.user).await?;

    let membership = find_membership::<O::Members>(db, owner, target_user.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(O::RESOURCE))?;

    let previous_role_id = <O::Members as MemberTable>::fields(&membership).role;
    let role = find_role::<O::Roles>(db, owner, &body.role).await?;

    update_row::<O::Members>(
        db,
        <O::Members as MemberTable>::assign_role(membership, role.id),
    )
    .await?;

    audit.write(O::ACTIONS.member_role_change, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "target_user_id": target_user.id.to_string(), "previous_role_id": previous_role_id.to_string(), "new_role": role.name, }))
    .await;

    Ok(ok_json("User role updated".to_string()))
}

pub async fn remove<O: Owner>(
    scope: O::ManageMembersScope,
    audit: Audit,
    Json(body): Json<RemoveMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let owner = scope.id();
    let db = &scope.db().web_db;
    let target_user = find_user_by_username(db, &body.user).await?;

    let membership = find_membership::<O::Members>(db, owner, target_user.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(O::RESOURCE))?;

    O::ensure_member_removable(db, owner, &membership).await?;

    delete_row::<O::Members>(db, membership).await?;

    audit.write(O::ACTIONS.member_remove, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "target_user_id": target_user.id.to_string(), }))
    .await;

    Ok(ok_json(O::MEMBER_REMOVED.to_string()))
}
