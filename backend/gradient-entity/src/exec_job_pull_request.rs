/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ExecJobId, ExecJobPullRequestId, IntegrationId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job_pull_request")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ExecJobPullRequestId,
    pub exec_job: ExecJobId,
    pub integration: IntegrationId,
    pub url: String,
    pub branch: String,
    pub number: i64,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::exec_job::Entity",
        from = "Column::ExecJob",
        to = "super::exec_job::Column::Id",
        on_delete = "Cascade"
    )]
    ExecJob,
    #[sea_orm(
        belongs_to = "super::integration::Entity",
        from = "Column::Integration",
        to = "super::integration::Column::Id",
        on_delete = "Cascade"
    )]
    Integration,
}

impl Related<super::exec_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExecJob.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
