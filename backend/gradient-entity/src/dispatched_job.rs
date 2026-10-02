/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ClusterAttemptId, DispatchedJobId, EvaluationId, ProjectId, TaskId};

#[repr(i16)]
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
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum DispatchedJobKind {
    #[default]
    #[sea_orm(num_value = 0)]
    Eval = 0,
    #[sea_orm(num_value = 1)]
    Build = 1,
}

#[repr(i16)]
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
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum DispatchedJobOutcome {
    #[default]
    #[sea_orm(num_value = 0)]
    Completed = 0,
    #[sea_orm(num_value = 1)]
    Failed = 1,
    /// `Abandoned` must not count towards failure rates or history scoring. The build may have
    /// succeeded before the worker disconnected or the server restarted.
    #[sea_orm(num_value = 2)]
    Abandoned = 2,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "dispatched_job")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DispatchedJobId,
    pub kind: DispatchedJobKind,
    pub evaluation_id: EvaluationId,
    pub project: ProjectId,
    pub task: Option<TaskId>,
    pub worker_id: String,
    pub job_id: Option<String>,
    pub score: f64,
    pub queued_at: NaiveDateTime,
    pub ready_at: Option<NaiveDateTime>,
    pub dispatched_at: NaiveDateTime,
    pub finished_at: Option<NaiveDateTime>,
    pub outcome: Option<DispatchedJobOutcome>,
    pub score_breakdown: Json,
    pub worker_context: Json,
    pub job_context: Json,
    pub instance_context: Option<Json>,
    pub candidates: Option<Json>,
    pub created_at: NaiveDateTime,
    /// These three scores are lifted out of `job_context` for the window index to carry. Averaging
    /// them from the jsonb was reading every window row from the heap. `AVG` is skipping the `None`
    /// of older rows like an absent json key.
    pub missing_nar_size: Option<i64>,
    pub missing_count: Option<i32>,
    pub dependency_count: Option<i32>,
    pub cluster_attempt: Option<ClusterAttemptId>,
    pub worker_elapsed_ms: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
