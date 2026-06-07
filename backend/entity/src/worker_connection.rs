/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OrganizationId, WorkerConnectionId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "worker_connection")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: WorkerConnectionId,
    pub worker_id: String,
    pub organization: OrganizationId,
    pub display_name: String,
    pub connected_at: NaiveDateTime,
    pub disconnected_at: Option<NaiveDateTime>,
    pub capabilities: Json,
    pub reason: Option<i16>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
