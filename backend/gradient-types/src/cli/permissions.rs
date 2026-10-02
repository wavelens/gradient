/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(ValueEnum, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
pub enum CreatePermission {
    /// Nobody via the API. Only the declarative state import may create them.
    None,
    /// Superusers only.
    Superusers,
    /// Any authenticated user.
    #[default]
    Everyone,
}

#[derive(Args, Debug, Clone, Default)]
pub struct PermissionsArgs {
    /// Who may create projects through the API: `none` (only the declarative state), `superusers`,
    /// or `everyone` (any authenticated user).
    #[arg(long = "permissions-create-project", value_enum, env = "GRADIENT_PERMISSIONS_CREATE_PROJECT", default_value_t = CreatePermission::Everyone)]
    pub create_project: CreatePermission,
    /// Who may create caches through the API: `none` (only the declarative state), `superusers`,
    /// or `everyone` (any authenticated user).
    #[arg(long = "permissions-create-cache", value_enum, env = "GRADIENT_PERMISSIONS_CREATE_CACHE", default_value_t = CreatePermission::Everyone)]
    pub create_cache: CreatePermission,
}
