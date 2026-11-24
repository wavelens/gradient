/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
pub use gradient_api_types::CreatePermission;
use gradient_derive::config_group;

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
    )]
    pub create_team: CreatePermission,
}
