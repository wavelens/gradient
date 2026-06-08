/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BuildId, DerivationId, DispatchedJobId, EvaluationId, OrganizationId, ProjectId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "dispatched_job")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DispatchedJobId,
    pub kind: i16,
    pub build_id: Option<BuildId>,
    pub evaluation_id: EvaluationId,
    pub organization: OrganizationId,
    pub project: Option<ProjectId>,
    pub derivation: Option<DerivationId>,
    pub worker_id: String,
    pub score: f64,
    pub queued_at: NaiveDateTime,
    pub ready_at: Option<NaiveDateTime>,
    pub dispatched_at: NaiveDateTime,
    pub finished_at: Option<NaiveDateTime>,
    pub outcome: Option<i16>,
    pub score_breakdown: Json,
    pub worker_context: Json,
    pub job_context: Json,
    pub instance_context: Option<Json>,
    pub candidates: Option<Json>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
