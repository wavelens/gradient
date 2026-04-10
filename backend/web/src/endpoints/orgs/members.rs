/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::db::{get_any_organization_by_name, get_organization_by_name};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect,
    RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

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

pub async fn get_organization_users(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<StringListItem>>>> {
    let organization: MOrganization =
        get_any_organization_by_name(state.0.clone(), organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if !organization.public {
        match &maybe_user {
            Some(user) => {
                if !user_is_org_member(&state.0, user.id, organization.id).await? {
                    return Err(WebError::not_found("Organization"));
                }
            }
            None => return Err(WebError::not_found("Organization")),
        }
    }

    let organization_users = EOrganizationUser::find()
        .join(JoinType::InnerJoin, ROrganizationUser::User.def())
        .select_also(entity::user::Entity)
        .filter(COrganizationUser::Organization.eq(organization.id))
        .all(&state.db)
        .await?;

    let role_ids: Vec<Uuid> = organization_users.iter().map(|(ou, _)| ou.role).collect();
    let role_map: std::collections::HashMap<Uuid, String> = ERole::find()
        .filter(CRole::Id.is_in(role_ids))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    let organization_users: Vec<StringListItem> = organization_users
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

    let res = BaseResponse {
        error: false,
        message: organization_users,
    };

    Ok(Json(res))
}

pub async fn post_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("User"))?;

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(target_user.id)),
        )
        .one(&state.db)
        .await?;

    if organization_user.is_some() {
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
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Role"))?;

    let organization_user = AOrganizationUser {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        user: Set(target_user.id),
        role: Set(role.id),
    };

    organization_user.insert(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User invited".to_string(),
    };

    Ok(Json(res))
}

pub async fn patch_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("User"))?;

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(target_user.id)),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("User not in Organization".to_string()))?;

    let role = ERole::find()
        .filter(CRole::Name.eq(body.role.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Role"))?;

    let mut aorganization_user: AOrganizationUser = organization_user.into();
    aorganization_user.role = Set(role.id);
    aorganization_user.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User role updated".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<RemoveUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("User"))?;

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(target_user.id)),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("User not in Organization".to_string()))?;

    let aorganization_user: AOrganizationUser = organization_user.into();
    aorganization_user.delete(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User kicked".to_string(),
    };

    Ok(Json(res))
}
