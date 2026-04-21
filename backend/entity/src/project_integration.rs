/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_integration")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project: Uuid,
    pub inbound_integration: Option<Uuid>,
    pub outbound_integration: Option<Uuid>,
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
        from = "Column::InboundIntegration",
        to = "super::integration::Column::Id"
    )]
    InboundIntegration,
    #[sea_orm(
        belongs_to = "super::integration::Entity",
        from = "Column::OutboundIntegration",
        to = "super::integration::Column::Id"
    )]
    OutboundIntegration,
}

impl ActiveModelBehavior for ActiveModel {}
