/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{GitHostConnectionId, IncomingWebhookDeliveryId};
pub use gradient_api_types::DeliveryResult;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "incoming_webhook_delivery")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: IncomingWebhookDeliveryId,
    pub git_host_connection: GitHostConnectionId,
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
        belongs_to = "super::git_host_connection::Entity",
        from = "Column::GitHostConnection",
        to = "super::git_host_connection::Column::Id",
        on_delete = "Cascade"
    )]
    GitHostConnection,
}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for gradient_api_types::IncomingWebhookDeliveryResponse {
    fn from(m: Model) -> Self {
        Self {
            id: m.id,
            event: m.event,
            status_code: m.status_code,
            result: m.result,
            detail: m.detail,
            repository: m.repository,
            tasks_scanned: m.tasks_scanned,
            queued: m.queued,
            created_at: m.created_at,
        }
    }
}
