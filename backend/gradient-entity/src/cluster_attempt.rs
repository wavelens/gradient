/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One try of a cluster job; at most one is open per cluster. `started_at` stays
//! `None` until every member accepted its assignment.

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ClusterAttemptId, ClusterJobId};

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
pub enum ClusterAttemptOutcome {
    #[default]
    #[sea_orm(num_value = 0)]
    Succeeded = 0,
    #[sea_orm(num_value = 1)]
    Failed = 1,
    #[sea_orm(num_value = 2)]
    PrepareFailed = 2,
    #[sea_orm(num_value = 3)]
    Aborted = 3,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cluster_attempt")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ClusterAttemptId,
    pub cluster_job: ClusterJobId,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub outcome: Option<ClusterAttemptOutcome>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cluster_job::Entity",
        from = "Column::ClusterJob",
        to = "super::cluster_job::Column::Id",
        on_delete = "Cascade"
    )]
    ClusterJob,
}

impl ActiveModelBehavior for ActiveModel {}
