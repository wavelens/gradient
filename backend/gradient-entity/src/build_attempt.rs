/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BuildAttemptId, BuildJobId, DerivationBuildId, DispatchedJobId};

#[repr(i32)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum AttemptOutcome {
    #[default]
    #[sea_orm(num_value = 0)]
    Running = 0,
    #[sea_orm(num_value = 1)]
    Built = 1,
    #[sea_orm(num_value = 2)]
    Substituted = 2,
    #[sea_orm(num_value = 3)]
    Failed = 3,
    #[sea_orm(num_value = 4)]
    Aborted = 4,
}

#[repr(i32)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum AttemptFailureReason {
    #[sea_orm(num_value = 0)]
    SubstituteUnavailable = 0,
    #[sea_orm(num_value = 1)]
    Oom = 1,
    #[sea_orm(num_value = 2)]
    DiskFull = 2,
    #[sea_orm(num_value = 3)]
    Network = 3,
    #[sea_orm(num_value = 4)]
    BuilderCrash = 4,
    #[sea_orm(num_value = 5)]
    BuilderNonzero = 5,
    #[sea_orm(num_value = 6)]
    WallClockTimeout = 6,
    #[sea_orm(num_value = 7)]
    SilentTimeout = 7,
    #[sea_orm(num_value = 8)]
    InputsUnavailable = 8,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_attempt")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BuildAttemptId,
    /// The eval that drove this attempt's dispatch. `None` once that evaluation
    /// is GC'd: the attempt (and its log) live on with the `derivation_build`
    /// anchor, its true owner, until the derivation itself is reclaimed.
    pub build_job: Option<BuildJobId>,
    pub derivation_build: DerivationBuildId,
    pub dispatched_job: DispatchedJobId,
    pub substitute: bool,
    pub outcome: AttemptOutcome,
    pub reason: Option<AttemptFailureReason>,
    pub failure_message: Option<String>,
    pub build_context: Json,
    pub build_started_at: Option<NaiveDateTime>,
    pub build_finished_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

impl Model {
    pub fn duration_ms(&self) -> Option<i64> {
        match (self.build_started_at, self.build_finished_at) {
            (Some(s), Some(f)) => Some((f - s).num_milliseconds().max(0)),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::build_job::Entity",
        from = "Column::BuildJob",
        to = "super::build_job::Column::Id",
        on_delete = "SetNull"
    )]
    BuildJob,
    #[sea_orm(
        belongs_to = "super::derivation_build::Entity",
        from = "Column::DerivationBuild",
        to = "super::derivation_build::Column::Id",
        on_delete = "Cascade"
    )]
    DerivationBuild,
    #[sea_orm(
        belongs_to = "super::dispatched_job::Entity",
        from = "Column::DispatchedJob",
        to = "super::dispatched_job::Column::Id"
    )]
    DispatchedJob,
}

impl ActiveModelBehavior for ActiveModel {}
