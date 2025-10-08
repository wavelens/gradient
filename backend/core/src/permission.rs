/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, EntityTrait, IntoActiveModel,
    QueryFilter,
};
use std::sync::Arc;

use super::types::*;

#[derive(Copy, Clone, Debug)]
pub enum Permission {
    View = 0,
    Edit = 1,
}

fn get_permission_bit(permissions: i64, permission: Permission) -> bool {
    permissions & (1 << permission as i64) != 0
}

fn set_permission_bit(permissions: i64, permission: Permission, value: bool) -> i64 {
    if value {
        permissions | (1 << permission as i64)
    } else {
        permissions & !(1 << permission as i64)
    }
}

pub async fn set_permission(
    state: Arc<ServerState>,
    role: MRole,
    permission: Permission,
    value: bool,
) -> Result<()> {
    if get_permission_bit(role.permission, permission) == value {
        return Ok(());
    }

    let mut arole = role.clone().into_active_model();
    arole.permission = Set(set_permission_bit(role.permission, permission, value));
    arole
        .save(&state.db)
        .await
        .context("Failed to save role permission")?;
    Ok(())
}

pub async fn get_permission(
    state: Arc<ServerState>,
    organization: MOrganization,
    user: MUser,
    permission: Permission,
) -> Result<bool> {
    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(user.id)),
        )
        .one(&state.db)
        .await
        .context("Failed to query organization user")?
        .ok_or_else(|| anyhow::anyhow!("User not found in organization"))?;

    let role = ERole::find_by_id(organization_user.role)
        .one(&state.db)
        .await
        .context("Failed to query user role")?
        .ok_or_else(|| anyhow::anyhow!("Role not found"))?;

    Ok(get_permission_bit(role.permission, permission))
}
