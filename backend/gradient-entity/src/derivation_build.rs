/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Global build-once anchor: one durable build-state row per derivation
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
    pub substitutable: bool,
    pub substituted: bool,
    /// True once this anchor's dependency edges have been flushed (at the
    /// flushing eval's completion). Promotion and dispatch gate on it: an anchor
    /// created mid-stream by a still-running, failed, or interrupted eval has no
    /// edges yet and must not be promoted as if it were dependency-free.
    pub edges_complete: bool,
    /// True when `flush_deferred_deps` could not resolve one of this anchor's
    /// declared dependency edges (the dependency derivation was never recorded).
    /// Its edge set is known-incomplete, so `mark_edges_complete_for_eval` refuses
    /// to promote it - otherwise a build_job with zero recorded edges would be
    /// dispatched as dependency-free and fail `InputsUnavailable`. Cleared when a
    /// later eval resolves every edge.
    pub edges_unresolved: bool,
    /// True once this anchor reached a terminal-success status (Completed /
    /// Substituted) AND every output's full runtime closure is present in our
    /// cache. Dispatch gates dependents on it: a dep marked done whose closure
    /// is incomplete would otherwise strand the dependent on `InputsUnavailable`.
    pub closure_complete: bool,
    /// True once this anchor's own `.drv` is in our cache AND every build
    /// dependency is itself `drv_closure_cached` - i.e. the `.drv`'s full
    /// transitive reference closure (input `.drv`s + input sources) is cached.
    /// The `.drv`-closure analogue of `closure_complete` (which tracks OUTPUTs):
    /// a worker can't import a build target's `.drv` until this holds, so
    /// dispatch gates non-substitutable anchors on it to stop racing the eval's
    /// progressive `.drv` push.
    pub drv_closure_cached: bool,
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
