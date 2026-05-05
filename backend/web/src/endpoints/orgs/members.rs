/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, OrgAccess, load_org};
use crate::helpers::{OptionExt, ok_json};
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect,
    RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringListItem {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AddUserRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveUserRequest {
    pub user: String,
}

// ── Access helpers ────────────────────────────────────────────────────────────

async fn find_user_by_username(state: &Arc<ServerState>, username: &str) -> WebResult<MUser> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(&state.web_db)
        .await?
        .or_not_found("User")
}

async fn find_org_membership(
    state: &Arc<ServerState>,
    org_id: OrganizationId,
    user_id: UserId,
) -> WebResult<Option<MOrganizationUser>> {
    Ok(EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(org_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.web_db)
        .await?)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn get_organization_users(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<StringListItem>>>> {
    let organization = load_org(
        &state.0,
        Caller::from_option(&maybe_user),
        organization,
        OrgAccess::Readable { label: "Organization" },
    )
    .await?;

    let organization_users = EOrganizationUser::find()
        .join(JoinType::InnerJoin, entity::organization_user::Relation::User.def())
        .select_also(entity::user::Entity)
        .filter(COrganizationUser::Organization.eq(organization.id))
        .all(&state.web_db)
        .await?;

    let role_ids: Vec<RoleId> = organization_users.iter().map(|(ou, _)| ou.role).collect();
    let role_map: std::collections::HashMap<RoleId, String> = ERole::find()
        .filter(CRole::Id.is_in(role_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    let items: Vec<StringListItem> = organization_users
        .iter()
        .map(|(ou, user)| StringListItem {
            id: user
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| ou.user.to_string()),
            name: role_map
                .get(&ou.role)
                .cloned()
                .unwrap_or_else(|| ou.role.to_string()),
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn post_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization = load_org(
        &state,
        Caller::User(&user),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    if find_org_membership(&state, organization.id, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists("User already in Organization"));
    }

    let role = ERole::find()
        .filter(
            Condition::all().add(CRole::Name.eq(body.role.clone())).add(
                Condition::any()
                    .add(CRole::Organization.eq(organization.id))
                    .add(CRole::Organization.is_null()),
            ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    AOrganizationUser {
        id: Set(OrganizationUserId::now_v7()),
        organization: Set(organization.id),
        user: Set(target_user.id),
        role: Set(role.id),
    }
    .insert(&state.web_db)
    .await?;

    Ok(ok_json("User invited".to_string()))
}

pub async fn patch_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization = load_org(
        &state,
        Caller::User(&user),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    let membership = find_org_membership(&state, organization.id, target_user.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Organization"))?;

    let role = ERole::find()
        .filter(CRole::Name.eq(body.role.clone()))
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    let mut active: AOrganizationUser = membership.into();
    active.role = Set(role.id);
    active.update(&state.web_db).await?;

    Ok(ok_json("User role updated".to_string()))
}

pub async fn delete_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<RemoveUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization = load_org(
        &state,
        Caller::User(&user),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    let membership = find_org_membership(&state, organization.id, target_user.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Organization"))?;

    let active: AOrganizationUser = membership.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json("User kicked".to_string()))
}
