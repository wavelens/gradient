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

/// Server-level worker available to any org that opts in via
/// `organization_base_worker`. Provisioned only from declarative state.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "base_worker")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BaseWorkerId,
    #[sea_orm(unique)]
    pub worker_id: String,
    pub token_hash: String,
    pub url: Option<String>,
    pub display_name: String,
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    /// Global gate. When false the base worker is off for every org.
    pub enabled: bool,
    /// Optional fixed auth identity used instead of per-org challenge.
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
