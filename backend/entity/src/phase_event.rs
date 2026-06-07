/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::PhaseEventId;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "phase_event")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: PhaseEventId,
    pub subject_kind: i16,
    pub subject_id: Uuid,
    pub phase: i16,
    pub event: i16,
    pub at: NaiveDateTime,
    pub worker_id: Option<String>,
    pub detail: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
