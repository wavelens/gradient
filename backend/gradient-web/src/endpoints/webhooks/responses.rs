/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::webhook::WebhookScope;
use gradient_types::*;
use serde::Serialize;

#[derive(Serialize, Debug)]
pub struct WebhookResponse {
    pub id: WebhookId,
    pub scope: WebhookScope,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub active: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

impl From<MWebhook> for WebhookResponse {
    fn from(m: MWebhook) -> Self {
        Self {
            id: m.id,
            scope: m.scope,
            name: m.name,
            url: m.url,
            events: serde_json::from_value(m.events).unwrap_or_default(),
            active: m.active,
            last_fired_at: m.last_fired_at,
            created_by: m.created_by,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Serialize, Debug)]
pub struct CreateWebhookResponse {
    pub webhook: WebhookResponse,
    pub secret: String,
}

#[derive(Serialize, Debug)]
pub struct DeliveryListItem<Id> {
    pub id: Id,
    pub event: String,
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub duration_ms: i32,
    pub delivered_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Debug)]
pub struct DeliveryDetail<Id> {
    #[serde(flatten)]
    pub item: DeliveryListItem<Id>,
    pub request_body: String,
    pub response_body: Option<String>,
}

impl From<MWebhookDelivery> for DeliveryListItem<WebhookDeliveryId> {
    fn from(r: MWebhookDelivery) -> Self {
        Self {
            id: r.id,
            event: r.event,
            success: r.success,
            response_status: r.response_status,
            error_message: r.error_message,
            duration_ms: r.duration_ms,
            delivered_at: r.delivered_at,
        }
    }
}

impl From<MWebhookDelivery> for DeliveryDetail<WebhookDeliveryId> {
    fn from(r: MWebhookDelivery) -> Self {
        Self {
            request_body: r.request_body.clone(),
            response_body: r.response_body.clone(),
            item: r.into(),
        }
    }
}

impl From<MTaskActionDelivery> for DeliveryListItem<TaskActionDeliveryId> {
    fn from(r: MTaskActionDelivery) -> Self {
        Self {
            id: r.id,
            event: r.event,
            success: r.success,
            response_status: r.response_status,
            error_message: r.error_message,
            duration_ms: r.duration_ms,
            delivered_at: r.delivered_at,
        }
    }
}

impl From<MTaskActionDelivery> for DeliveryDetail<TaskActionDeliveryId> {
    fn from(r: MTaskActionDelivery) -> Self {
        Self {
            request_body: r.request_body.clone(),
            response_body: r.response_body.clone(),
            item: r.into(),
        }
    }
}
