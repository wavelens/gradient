/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::FeatureId;

#[derive(
    Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize, Default,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
pub enum FeatureKind {
    #[default]
    #[sea_orm(string_value = "feature")]
    Feature,
    #[sea_orm(string_value = "architecture")]
    Architecture,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "system_requirement")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: FeatureId,
    pub name: String,
    pub kind: FeatureKind,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
