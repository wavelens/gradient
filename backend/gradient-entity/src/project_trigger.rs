/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, ProjectTriggerId};

/// What fires an evaluation: repo polling, a forge push/PR webhook, or a cron
/// schedule. Tags the polymorphic `config` jsonb column.
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
pub enum TriggerType {
    #[default]
    #[sea_orm(num_value = 0)]
    Polling = 0,
    #[sea_orm(num_value = 1)]
    ReporterPush = 1,
    #[sea_orm(num_value = 2)]
    ReporterPullRequest = 2,
    #[sea_orm(num_value = 3)]
    Time = 3,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_trigger")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: ProjectTriggerId,
    pub project: ProjectId,
    pub trigger_type: TriggerType,
    pub config: Json,
    pub active: bool,
    pub last_fired_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id"
    )]
    Project,
}

impl ActiveModelBehavior for ActiveModel {}
