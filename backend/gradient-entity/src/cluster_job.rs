/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::ClusterJobId;

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
pub enum ClusterJobStatus {
    #[default]
    #[sea_orm(num_value = 0)]
    Queued = 0,
    #[sea_orm(num_value = 1)]
    Running = 1,
    #[sea_orm(num_value = 2)]
    Completed = 2,
    #[sea_orm(num_value = 3)]
    Failed = 3,
    #[sea_orm(num_value = 4)]
    Aborted = 4,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cluster_job")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ClusterJobId,
    pub status: ClusterJobStatus,
    pub same_zone: bool,
    pub attempts: i32,
    pub retry_budget: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
