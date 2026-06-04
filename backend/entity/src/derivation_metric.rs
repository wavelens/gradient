/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationId, DerivationMetricId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_metric")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationMetricId,
    pub derivation: DerivationId,
    pub pname: Option<String>,
    pub closure_size: Option<i64>,
    pub peak_ram_mb: Option<i64>,
    pub cpu_time_ms: Option<i64>,
    pub avg_cpu_pct: Option<f64>,
    pub disk_read_bytes: Option<i64>,
    pub disk_write_bytes: Option<i64>,
    pub oom_killed: bool,
    pub build_time_ms: Option<i64>,
    pub worker_id: String,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id"
    )]
    Derivation,
}

impl ActiveModelBehavior for ActiveModel {}
