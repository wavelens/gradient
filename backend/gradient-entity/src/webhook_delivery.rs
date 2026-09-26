/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{WebhookDeliveryId, WebhookId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "webhook_delivery")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: WebhookDeliveryId,
    pub webhook_id: WebhookId,
    pub event: String,
    #[sea_orm(column_type = "Text")]
    pub request_body: String,
    pub response_status: Option<i32>,
    #[sea_orm(column_type = "Text", nullable)]
    pub response_body: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub error_message: Option<String>,
    pub success: bool,
    pub duration_ms: i32,
    pub delivered_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::webhook::Entity",
        from = "Column::WebhookId",
        to = "super::webhook::Column::Id",
        on_delete = "Cascade"
    )]
    Webhook,
}

impl Related<super::webhook::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Webhook.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
