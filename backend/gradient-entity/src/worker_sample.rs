/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OrganizationId, WorkerSampleId};

/// Worker lifecycle state at sample time; today a typed draining flag.
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
pub enum WorkerSampleState {
    #[default]
    #[sea_orm(num_value = 0)]
    Active = 0,
    #[sea_orm(num_value = 1)]
    Draining = 1,
}

impl From<bool> for WorkerSampleState {
    fn from(draining: bool) -> Self {
        if draining {
            Self::Draining
        } else {
            Self::Active
        }
    }
}

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
    pub state: WorkerSampleState,
    pub capabilities: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
