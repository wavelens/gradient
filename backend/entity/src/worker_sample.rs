/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OrganizationId, WorkerSampleId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "worker_sample")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: WorkerSampleId,
    pub worker_id: String,
    pub organization: OrganizationId,
    pub at: NaiveDateTime,
    pub cpu_usage_pct: Option<f32>,
    pub ram_free_mb: Option<i64>,
    pub ram_total_mb: Option<i64>,
    pub disk_speed_mbps: Option<f32>,
    pub network_speed_mbps: Option<f32>,
    pub assigned_jobs: i32,
    pub max_concurrent_builds: i32,
    pub state: i16,
    pub capabilities: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
