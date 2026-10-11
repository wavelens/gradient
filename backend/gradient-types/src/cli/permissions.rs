/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::{Args, ValueEnum};
use gradient_derive::config_group;
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

#[config_group("permissions")]
#[derive(Args, Debug, Clone)]
pub struct PermissionsArgs {
    /// Who can create projects through the API: `none` (only the declarative state),
    /// `superusers`, or `everyone` (any authenticated user).
    #[arg(value_enum, default_value_t = CreatePermission::Everyone)]
    pub create_project: CreatePermission,
    /// Who can create caches through the API: `none` (only the declarative state),
    /// `superusers`, or `everyone` (any authenticated user).
    #[arg(value_enum, default_value_t = CreatePermission::Everyone)]
    pub create_cache: CreatePermission,
    /// Who can create teams through the API: `none` (only the declarative state),
    /// `superusers`, or `everyone` (any authenticated user).
    #[arg(
        value_enum,
        default_value_t = CreatePermission::Everyone,
        help = "Who may create teams through the API: `none` (only the declarative state), `superusers`, or `everyone` (any authenticated user).",
    )]
    pub create_team: CreatePermission,
}
