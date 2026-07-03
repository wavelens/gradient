/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::PhaseEventId;

/// Discriminant tagging the polymorphic `subject_id`: a `derivation_build.id`
/// or an `evaluation.id`.
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
pub enum PhaseSubjectKind {
    #[default]
    #[sea_orm(num_value = 0)]
    Build = 0,
    #[sea_orm(num_value = 1)]
    Evaluation = 1,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "phase_event")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: PhaseEventId,
    pub subject_kind: PhaseSubjectKind,
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
