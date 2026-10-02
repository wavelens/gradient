/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::{BaseWorkerId, UserId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "base_worker")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BaseWorkerId,
    #[sea_orm(unique)]
    pub worker_id: String,
    pub token_hash: String,
    #[serde(skip_serializing)]
    pub token_encrypted: Option<String>,
    pub url: Option<String>,
    pub display_name: String,
    pub gradient_ci: bool,
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    pub enabled: bool,
    /// A true value is enabling this worker on every newly created project. A later opt-out from
    /// the UI is kept.
    pub auto_enable: bool,
    pub authorize_against: Option<Uuid>,
    pub created_by: Option<UserId>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
}

impl ActiveModelBehavior for ActiveModel {}
