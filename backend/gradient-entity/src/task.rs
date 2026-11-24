/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvaluationId, GitHostConnectionId, ProjectId, TaskId, UserId};
pub use gradient_api_types::ConcurrencyPolicy;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "task")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: TaskId,
    pub project: ProjectId,
    #[sea_orm(indexed)]
    pub name: String,
    pub active: bool,
    pub display_name: String,
    #[sea_orm(column_type = "Text")]
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    pub last_evaluation: Option<EvaluationId>,
    pub last_check_at: NaiveDateTime,
    pub force_evaluation: bool,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    pub keep_evaluations: i32,
    pub concurrency: ConcurrencyPolicy,
    pub wait_for_workers: bool,
    pub retry_failed_builds: bool,
    pub multi_node_evaluation: bool,
    pub exec_enabled: bool,
    pub only_evaluate: bool,
    pub restrict_evaluation_fetches: bool,
    pub extra_allowed_uri_prefixes: Vec<String>,
    pub last_fetch_at: Option<NaiveDateTime>,
    pub last_fetch_ok_at: Option<NaiveDateTime>,
    #[sea_orm(column_type = "Text", nullable)]
    pub last_fetch_error: Option<String>,
    pub git_host_connection: Option<GitHostConnectionId>,
    pub report_status: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::LastEvaluation",
        to = "super::evaluation::Column::Id"
    )]
    LastEvaluation,
}

impl ActiveModelBehavior for ActiveModel {}

impl From<&Model> for gradient_api_types::TaskRepositoryFetchStatus {
    fn from(task: &Model) -> Self {
        match (task.last_fetch_at, &task.last_fetch_error) {
            (None, _) => Self::Unchecked,
            (Some(_), Some(_)) => Self::Failing,
            (Some(_), None) => Self::Verified,
        }
    }
}
