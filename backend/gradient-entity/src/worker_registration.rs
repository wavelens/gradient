/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, UserId, WorkerRegistrationId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "worker_registration")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: WorkerRegistrationId,
    pub peer_id: ProjectId,
    pub worker_id: String,
    pub token_hash: String,
    #[serde(skip_serializing)]
    pub token_encrypted: Option<String>,
    pub managed: bool,
    /// The server is connecting outbound to this URL when set, instead of waiting for an inbound
    /// connection.
    pub url: Option<String>,
    pub active: bool,
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    pub display_name: String,
    pub gradient_ci: bool,
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
