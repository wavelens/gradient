/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    MemberTable, Owner, OwnerScope, RoleRow, RoleTable, active, delete_row, insert_unique_row,
    update_unique_row,
};
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::lookup::{require, require_for_delete};
use crate::names::RoleName;
use crate::permissions::PermissionEntry;
use crate::refusal::{Change, SharedRefusal};
use crate::resource::Resource;
use axum::Json;
use gradient_db::WebDb;
use gradient_types::*;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug)]
pub struct RoleResponse<F> {
    pub id: RoleId,
    pub name: String,
    #[serde(flatten)]
    pub owner: F,
    pub builtin: bool,
    pub managed: bool,
    pub permissions: Vec<&'static str>,
}

#[derive(Serialize, Debug)]
pub struct RoleListResponse<F> {
    pub roles: Vec<RoleResponse<F>>,
    pub available_permissions: Vec<PermissionEntry>,
}

#[derive(Deserialize, Debug)]
pub struct CreateRoleRequest {
    pub name: RoleName,
    pub permissions: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct PatchRoleRequest {
    pub name: Option<RoleName>,
    pub permissions: Option<Vec<String>>,
}

type OwnerField<O> = <<O as Owner>::Roles as RoleTable>::OwnerField;
type Role<O> = RoleResponse<OwnerField<O>>;

fn response<O: Owner>(row: &RoleRow<O>) -> Role<O> {
    let fields = <O::Roles as RoleTable>::fields(row);
    RoleResponse {
        id: fields.id,
        name: fields.name,
        owner: fields.owner.into(),
        builtin: O::is_builtin_role(fields.id),
        managed: fields.managed,
        permissions: O::permission_names(fields.permission),
    }
}

fn owned_by<O: Owner>(role: RoleRow<O>, owner: O::Id) -> WebResult<RoleRow<O>> {
    if let Some(role_owner) = <O::Roles as RoleTable>::fields(&role).owner
        && role_owner != owner
    {
        // Cross-owner access is treated as not-found to avoid leaking ids.
        return Err(WebError::not_found(Resource::Role));
    }

    Ok(role)
}

async fn load_role<O: Owner>(db: &WebDb, owner: O::Id, role_id: RoleId) -> WebResult<RoleRow<O>> {
    owned_by::<O>(require::<O::Roles>(db, role_id).await?, owner)
}

async fn name_clash<T: RoleTable>(
    db: &WebDb,
    owner: T::Id,
    name: &str,
    except: Option<RoleId>,
) -> WebResult<()> {
    let taken = gradient_db::roles::name_taken::<T, _>(
        db,
        T::name_column(),
        name,
        T::id_column(),
        except,
        T::owner_column(),
        owner,
    )
    .await?;
    if taken {
        return Err(WebError::already_exists(Resource::RoleName));
    }
    Ok(())
}

async fn assigned_to_members<T: MemberTable>(
    db: &impl ConnectionTrait,
    owner: T::Id,
    role_id: RoleId,
) -> WebResult<bool> {
    Ok(gradient_db::memberships::role_assigned::<T, _>(
        db,
        T::role_column(),
        role_id,
        T::owner_column(),
        owner,
    )
    .await?)
}

pub async fn list<O: Owner>(
    scope: O::ReadRolesScope,
) -> WebResult<Json<BaseResponse<RoleListResponse<OwnerField<O>>>>> {
    let roles = gradient_db::roles::builtin_or_owned::<O::Roles, _>(
        &scope.db().web_db,
        <O::Roles as RoleTable>::owner_column(),
        scope.id(),
    )
    .await?;

    Ok(ok_json(RoleListResponse {
        roles: roles.iter().map(response::<O>).collect(),
        available_permissions: O::available_permissions(),
    }))
}

pub async fn create<O: Owner>(
    scope: O::ManageRolesScope,
    audit: Audit,
    JsonBody(body): JsonBody<CreateRoleRequest>,
) -> WebResult<Json<BaseResponse<Role<O>>>> {
    let owner = scope.id();
    let db = &scope.db().web_db;
    let mask = O::parse_permissions(&body.permissions)?;

    // Names must be unique within the owner and must not collide with a built-in role name.
    // Membership lookup by name would become ambiguous otherwise.
    name_clash::<O::Roles>(db, owner, body.name.as_str(), None).await?;

    let role = insert_unique_row::<O::Roles>(
        db,
        <O::Roles as RoleTable>::new(RoleId::now_v7(), body.name.to_string(), owner, mask),
        Resource::RoleName,
    )
    .await?;
    let created = <O::Roles as RoleTable>::fields(&role);

    audit.write(O::ACTIONS.role_create, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "role_id": created.id.to_string(), "name": created.name, "permission_mask": mask, }))
    .await;

    Ok(ok_json(response::<O>(&role)))
}

pub async fn show<O: Owner>(scope: O::ReadRolesScope) -> WebResult<Json<BaseResponse<Role<O>>>> {
    let role_id: RoleId = scope.params().param("role_id")?;

    let role = load_role::<O>(&scope.db().web_db, scope.id(), role_id).await?;
    Ok(ok_json(response::<O>(&role)))
}

pub async fn update<O: Owner>(
    scope: O::ManageRolesScope,
    audit: Audit,
    JsonBody(body): JsonBody<PatchRoleRequest>,
) -> WebResult<Json<BaseResponse<Role<O>>>> {
    let owner = scope.id();
    let db = &scope.db().web_db;
    let role_id: RoleId = scope.params().param("role_id")?;

    let role = load_role::<O>(db, owner, role_id).await?;
    let current = <O::Roles as RoleTable>::fields(&role);

    if O::is_builtin_role(current.id) {
        return Err(SharedRefusal::BuiltinRole(Change::Modify).into());
    }

    if current.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Role).into());
    }

    let previous_mask = current.permission;
    let previous_name = current.name;
    let mut row = active::<O::Roles>(role);

    if let Some(name) = body.name {
        name_clash::<O::Roles>(db, owner, name.as_str(), Some(role_id)).await?;
        <O::Roles as RoleTable>::rename(&mut row, name.into_inner());
    }

    if let Some(perms) = body.permissions {
        <O::Roles as RoleTable>::set_permission(&mut row, O::parse_permissions(&perms)?);
    }

    let updated = update_unique_row::<O::Roles>(db, row, Resource::RoleName).await?;
    let fields = <O::Roles as RoleTable>::fields(&updated);

    audit.write(O::ACTIONS.role_update, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "role_id": fields.id.to_string(), "previous_name": previous_name, "previous_permission_mask": previous_mask, "new_name": fields.name, "new_permission_mask": fields.permission, }))
    .await;

    Ok(ok_json(response::<O>(&updated)))
}

pub async fn delete<O: Owner>(
    scope: O::ManageRolesScope,
    audit: Audit,
) -> WebResult<Json<BaseResponse<bool>>> {
    let owner = scope.id();
    let db = &scope.db().web_db;
    let role_id: RoleId = scope.params().param("role_id")?;

    let tx = db.inner().begin().await?;
    let role = owned_by::<O>(require_for_delete::<O::Roles>(&tx, role_id).await?, owner)?;
    let current = <O::Roles as RoleTable>::fields(&role);

    if O::is_builtin_role(current.id) {
        return Err(SharedRefusal::BuiltinRole(Change::Delete).into());
    }

    if current.managed {
        return Err(SharedRefusal::StateManaged(Change::Delete, Resource::Role).into());
    }

    if assigned_to_members::<O::Members>(&tx, owner, role_id).await? {
        return Err(SharedRefusal::RoleAssignedToMembers.into());
    }
    if O::role_granted_to_a_team(&tx, role_id).await? {
        return Err(SharedRefusal::RoleGrantedToTeam.into());
    }

    delete_row::<O::Roles>(&tx, role).await?;
    tx.commit().await?;

    audit.write(O::ACTIONS.role_delete, O::event_owner(owner), serde_json::json!({ (O::ID_KEY): owner.to_string(), "role_id": role_id.to_string(), "name": current.name, }))
    .await;

    Ok(ok_json(true))
}
