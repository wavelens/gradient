/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationBuildId, DerivationId};

pub use crate::build::BuildStatus;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_build")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationBuildId,
    #[sea_orm(unique)]
    pub derivation: DerivationId,
    pub status: BuildStatus,
    pub cache_available: bool,
    /// `probed` is set once the upstream probe answered, hit or miss. The need is not descending
    /// before that. Descending earlier would dispatch build inputs that a passthrough would make
    /// pointless. A job already handed to a worker cannot be recalled.
    pub probed: bool,
    pub substituted: bool,
    pub fetchable: bool,
    pub blocking_deps: i32,
    pub missing_runtime_deps: i32,
    /// `wanted` is true while an entry point or a wanted, named builder is still needing these
    /// outputs in our cache. `can_start::update_need` is updating it, and every arm of the
    /// promotion gate is reading it.
    pub wanted: bool,
    pub prioritized: bool,
    pub attempt: i32,
    pub timeout_secs: Option<i64>,
    pub max_silent_secs: Option<i64>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub queued_at: Option<NaiveDateTime>,
    pub ready_at: Option<NaiveDateTime>,
    pub dispatched_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id",
        on_delete = "Cascade"
    )]
    Derivation,
}

impl ActiveModelBehavior for ActiveModel {}
