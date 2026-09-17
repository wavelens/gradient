/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::OutboxId;

/// What a row owes. The first three are *events*: the effects actor expands one
/// into the per-action deliveries it implies. `ActionDelivery` is one external
/// call and nothing else.
#[repr(i16)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum OutboxKind {
    #[default]
    #[sea_orm(num_value = 0)]
    BuildStatus = 0,
    #[sea_orm(num_value = 1)]
    EvaluationStatus = 1,
    #[sea_orm(num_value = 2)]
    LogFinalize = 2,
    #[sea_orm(num_value = 3)]
    ActionDelivery = 3,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "outbox")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: OutboxId,
    pub kind: OutboxKind,
    #[sea_orm(column_type = "Text")]
    pub key: String,
    pub payload: Json,
    pub created_at: NaiveDateTime,
    pub attempts: i32,
    pub next_attempt_at: NaiveDateTime,
    pub delivered_at: Option<NaiveDateTime>,
    pub failed_at: Option<NaiveDateTime>,
    #[sea_orm(column_type = "Text", nullable)]
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
