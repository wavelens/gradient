/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, ProjectId, UserId, WebhookId};

/// Who owns a webhook and so which events reach it.
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
pub enum WebhookScope {
    #[default]
    #[sea_orm(num_value = 0)]
    Project = 0,
    #[sea_orm(num_value = 1)]
    Cache = 1,
    #[sea_orm(num_value = 2)]
    Instance = 2,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "webhook")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: WebhookId,
    pub scope: WebhookScope,
    pub project: Option<ProjectId>,
    pub cache: Option<CacheId>,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub events: Json,
    pub active: bool,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub last_fired_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::webhook_delivery::Entity")]
    Deliveries,
}

impl Related<super::webhook_delivery::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deliveries.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
