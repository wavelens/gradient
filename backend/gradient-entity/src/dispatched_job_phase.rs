/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DispatchedJobId, DispatchedJobPhaseId};

/// One worker phase span. `seq` is the span's position in the worker's report
/// and `parent_seq` points at the enclosing span, so the nesting survives
/// without a recursive type.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "dispatched_job_phase")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DispatchedJobPhaseId,
    pub dispatched_job: DispatchedJobId,
    pub seq: i32,
    pub parent_seq: Option<i32>,
    pub phase: i16,
    pub start_ms: i64,
    pub end_ms: i64,
    pub paths: i32,
    pub bytes: i64,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
