/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;

use serde::{Deserialize, Serialize};

#[repr(i16)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(
        sea_orm::DeriveActiveEnum,
        sea_orm::EnumIter,
        num_enum::IntoPrimitive,
        num_enum::TryFromPrimitive
    )
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "snake_case")]
pub enum WebhookScope {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Project = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Cache = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    Instance = 2,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Team = 3,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateWebhookRequest {
    pub name: String,
    #[schema(format = "uri")]
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Deserialize, Serialize, Debug, Default, utoipa::ToSchema)]
pub struct UpdateWebhookRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[schema(format = "uri")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct WebhookResponse {
    pub id: WebhookId,
    pub scope: WebhookScope,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub active: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub last_delivery: Option<WebhookNewestDeliveryAttemptSummary>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct WebhookNewestDeliveryAttemptSummary {
    pub success: bool,
    pub delivered_at: chrono::NaiveDateTime,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct RotatedSecret {
    pub secret: String,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateWebhookResponse {
    pub webhook: WebhookResponse,
    pub secret: String,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct DeliveryListItem<Id> {
    pub id: Id,
    pub event: String,
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub duration_ms: i32,
    pub delivered_at: chrono::NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct DeliveryDetail<Id> {
    #[serde(flatten)]
    pub item: DeliveryListItem<Id>,
    pub request_body: String,
    pub response_body: Option<String>,
}

pub type WebhookDeliveryItem = DeliveryListItem<WebhookDeliveryId>;
pub type ActionDeliveryItem = DeliveryListItem<TaskActionDeliveryId>;
