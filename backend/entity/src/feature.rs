/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Discriminates between a Nix system-feature (e.g. `"kvm"`, `"big-parallel"`)
/// and a Nix architecture / system string (e.g. `"x86_64-linux"`).
#[derive(
    Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize, Default,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
pub enum FeatureKind {
    /// A Nix system feature such as `"kvm"`, `"big-parallel"`, or `"nixos-test"`.
    #[default]
    #[sea_orm(string_value = "feature")]
    Feature,
    /// A Nix system / architecture string such as `"x86_64-linux"` or `"aarch64-linux"`.
    #[sea_orm(string_value = "architecture")]
    Architecture,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "system_requirement")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub name: String,
    pub kind: FeatureKind,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
