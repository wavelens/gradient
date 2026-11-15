/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationBuildId, DerivationId, ProjectId, YearlyBuildResourceRecordId};

#[repr(i16)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
    utoipa::ToSchema,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum YearlyBuildRecordMetric {
    #[default]
    #[sea_orm(num_value = 0)]
    BuildTime = 0,
    #[sea_orm(num_value = 1)]
    PeakRam = 1,
    #[sea_orm(num_value = 2)]
    CpuTime = 2,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "yearly_build_resource_record")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: YearlyBuildResourceRecordId,
    pub year: i32,
    pub kind: YearlyBuildRecordMetric,
    pub value: i64,
    pub derivation: Option<DerivationId>,
    pub derivation_name: String,
    pub derivation_build: Option<DerivationBuildId>,
    pub project: Option<ProjectId>,
    pub project_name: Option<String>,
    pub project_display_name: Option<String>,
    pub recorded_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
