/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-entry-point dependency-closure build-status histogram, maintained
//! incrementally as builds transition. One row per `(entry_point, status)`
//! holds the count of the entry point's closure builds currently in that
//! `BuildStatus`. Powers the project page's per-package segmented bar without
//! the per-request recursive closure walk.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EntryPointDepCountId, EntryPointId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "entry_point_dep_count")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EntryPointDepCountId,
    pub entry_point: EntryPointId,
    pub status: i32,
    pub count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::entry_point::Entity",
        from = "Column::EntryPoint",
        to = "super::entry_point::Column::Id"
    )]
    EntryPoint,
}

impl ActiveModelBehavior for ActiveModel {}
