/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::ExecJobId;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job_output")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub exec_job: ExecJobId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub path: String,
    pub size: i64,
    pub content_type: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
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
}

impl Related<super::exec_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExecJob.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for gradient_api_types::ExecJobOutputView {
    fn from(row: Model) -> Self {
        Self {
            path: row.path,
            size: row.size,
            content_type: row.content_type,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}
