/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CachedPathId, DebugInfoId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "debug_info")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DebugInfoId,
    pub build_id: String,
    pub cached_path: CachedPathId,
    pub member: String,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cached_path::Entity",
        from = "Column::CachedPath",
        to = "super::cached_path::Column::Id"
    )]
    CachedPath,
}

impl ActiveModelBehavior for ActiveModel {}
