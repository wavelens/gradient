/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Global build-once shared build: one durable build-state row per derivation
//! (UNIQUE on `derivation`). Per-eval scoring and logs live in `build_job` /
//! `build_attempt`; this row is the single source of truth for whether a
//! derivation has been built.

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
    /// The upstream probe has answered for this shared build, hit or miss. Until it
    /// has, the need stops here: descending into the build inputs of an output an
    /// upstream serves dispatches a closure the passthrough then makes pointless, and
    /// a job already handed to a worker cannot be recalled.
    pub probed: bool,
    pub substituted: bool,
    /// Builds wanting it can get this shared build's outputs: available in a cache, or terminal
    /// success with every output complete in our cache. Flipped by the event that
    /// changes it, with the parents' `blocking_deps` moved from that flip.
    pub fetchable: bool,
    /// Direct dependencies that are not fetchable. Zero is the can-start gate.
    pub blocking_deps: i32,
    /// Runtime dependencies that are not complete. Zero, with every output
    /// present, is complete.
    pub missing_runtime_deps: i32,
    /// Something still wants this shared build's outputs in our cache: an entry point
    /// names it, or a wanted, named builder depends on it. Updated by
    /// `can_start::update_need` on the events that change it; every arm of
    /// the promotion gate reads it.
    pub wanted: bool,
    /// Dispatches ahead of unprioritized work. Cleared by the database when the
    /// shared build fails permanently, dependency-fails, times out, is aborted or is skipped.
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
