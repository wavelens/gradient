/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{GithubInstallationId, ProjectId, UserId};

/// A GitHub App installation bound to a project.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "github_installation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: GithubInstallationId,
    pub project: ProjectId,
    pub installation_id: i64,
    #[sea_orm(column_type = "Text", nullable)]
    pub account_login: Option<String>,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
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
}

impl ActiveModelBehavior for ActiveModel {}
