/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, RoleId, TeamId, TeamProjectRequestId, UserId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "team_project_request")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: TeamProjectRequestId,
    pub team: TeamId,
    pub project: ProjectId,
    pub role: Option<RoleId>,
    pub includes_users: bool,
    pub includes_workers: bool,
    pub requested_by: Option<UserId>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
