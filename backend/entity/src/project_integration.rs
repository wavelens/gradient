/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{IntegrationId, ProjectId};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_integration")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project: ProjectId,
    pub outbound_integration: Option<IntegrationId>,
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
        belongs_to = "super::integration::Entity",
        from = "Column::OutboundIntegration",
        to = "super::integration::Column::Id"
    )]
    OutboundIntegration,
}

impl ActiveModelBehavior for ActiveModel {}
