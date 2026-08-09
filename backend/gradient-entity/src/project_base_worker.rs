/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BaseWorkerId, ProjectBaseWorkerId, ProjectId, UserId};

/// Per-project opt-in for a base worker. Row present means the project enabled it.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_base_worker")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ProjectBaseWorkerId,
    pub project: ProjectId,
    pub base_worker: BaseWorkerId,
    pub created_by: Option<UserId>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Project,
    BaseWorker,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Project => Entity::belongs_to(super::project::Entity)
                .from(Column::Project)
                .to(super::project::Column::Id)
                .into(),
            Self::BaseWorker => Entity::belongs_to(super::base_worker::Entity)
                .from(Column::BaseWorker)
                .to(super::base_worker::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
