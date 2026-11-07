/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{IncomingWebhookDeliveryId, IntegrationId};

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
    utoipa::ToSchema,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum DeliveryResult {
    #[sea_orm(num_value = 0)]
    Queued = 0,
    #[sea_orm(num_value = 1)]
    Skipped = 1,
    #[sea_orm(num_value = 2)]
    NoMatchingTask = 2,
    #[default]
    #[sea_orm(num_value = 3)]
    Ignored = 3,
    #[sea_orm(num_value = 4)]
    Refused = 4,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "incoming_webhook_delivery")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: IncomingWebhookDeliveryId,
    pub integration: IntegrationId,
    pub event: String,
    pub status_code: i16,
    pub result: DeliveryResult,
    #[sea_orm(column_type = "Text", nullable)]
    pub detail: Option<String>,
    pub repository: Option<String>,
    pub tasks_scanned: i32,
    pub queued: i32,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::integration::Entity",
        from = "Column::Integration",
        to = "super::integration::Column::Id",
        on_delete = "Cascade"
    )]
    Integration,
}

impl ActiveModelBehavior for ActiveModel {}
