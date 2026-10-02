/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::scope::{Verb, WebhookOwner};
use crate::audit::{RequestInfo, record as audit_record};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::Json;
use axum::extract::{Query, State};
use gradient_ci::actions::encrypt_action_secret;
use gradient_core::ServerState;
use gradient_entity::webhook::WebhookScope;
use gradient_types::events::{Envelope, EventFilter, webhook};
use gradient_types::ids::PendingDeliveryId;
use gradient_types::input::load_secret_bytes;
use gradient_types::*;
use gradient_util::http_validation::validate_webhook_url;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, Order, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

type ApiState = State<Arc<ServerState>>;

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

#[derive(Serialize, Debug)]
pub struct CreateWebhookResponse {
    pub webhook: WebhookResponse,
    pub secret: String,
}

#[derive(Deserialize, Debug)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Deserialize, Debug)]
pub struct UpdateWebhookRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

#[derive(Deserialize, Debug)]
pub struct DeliveryListQuery {
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Serialize, Debug)]
pub struct DeliveryListItem {
    pub id: WebhookDeliveryId,
    pub event: String,
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub duration_ms: i32,
    pub delivered_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Debug)]
pub struct DeliveryDetail {
    #[serde(flatten)]
    pub item: DeliveryListItem,
    pub request_body: String,
    pub response_body: Option<String>,
}

fn default_true() -> bool {
    true
}

fn to_response(m: MWebhook) -> WebhookResponse {
    WebhookResponse {
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

fn to_delivery_item(r: MWebhookDelivery) -> DeliveryListItem {
    DeliveryListItem {
        id: r.id,
        event: r.event,
        success: r.success,
        response_status: r.response_status,
        error_message: r.error_message,
        duration_ms: r.duration_ms,
        delivered_at: r.delivered_at,
    }
}

fn check_events(events: &[String]) -> WebResult<()> {
    EventFilter::validate(events).map_err(WebError::unprocessable_entity)
}

fn check_url(url: &str) -> WebResult<()> {
    validate_webhook_url(url)
        .map(drop)
        .map_err(|e| WebError::unprocessable_entity(e.to_string()))
}

fn new_secret(state: &ServerState) -> WebResult<(String, String)> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngExt as _;

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw);
    let plaintext = format!("whs_{}", URL_SAFE_NO_PAD.encode(raw));
    let key = load_secret_bytes(&state.config.secrets.crypt_file)
        .map_err(|e| WebError::internal(e.to_string()))?;
    let encrypted = encrypt_action_secret(&plaintext, key.expose())
        .map_err(|e| WebError::internal(e.to_string()))?;
    Ok((plaintext, encrypted))
}

async fn owned_webhook(state: &ServerState, owner: &WebhookOwner) -> WebResult<MWebhook> {
    let id: WebhookId = owner.param("id")?;
    EWebhook::find_by_id(id)
        .filter(owner.rows())
        .one(&state.web_db)
        .await?
        .or_not_found("Webhook")
}

async fn audit(
    state: &ServerState,
    owner: &WebhookOwner,
    verb: Verb,
    info: &RequestInfo,
    metadata: serde_json::Value,
) {
    audit_record(
        state,
        Some(owner.user.id),
        owner.action(verb),
        owner.event_owner(),
        info,
        Some(metadata),
    )
    .await;
}

pub async fn list_webhooks(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<Vec<WebhookResponse>>>> {
    let rows = EWebhook::find()
        .filter(owner.rows())
        .order_by_asc(CWebhook::Name)
        .all(&state.web_db)
        .await?;
    Ok(ok_json(rows.into_iter().map(to_response).collect()))
}

pub async fn create_webhook(
    State(state): ApiState,
    info: RequestInfo,
    owner: WebhookOwner,
    Json(body): Json<CreateWebhookRequest>,
) -> WebResult<Json<BaseResponse<CreateWebhookResponse>>> {
    check_url(&body.url)?;
    check_events(&body.events)?;
    let existing = EWebhook::find()
        .filter(owner.rows())
        .filter(CWebhook::Name.eq(body.name.clone()))
        .one(&state.web_db)
        .await?;
    if existing.is_some() {
        return Err(WebError::already_exists("Webhook"));
    }

    let (secret, encrypted) = new_secret(&state)?;
    let now = gradient_types::now();
    let row = MWebhook {
        id: WebhookId::now_v7(),
        scope: owner.scope,
        project: owner.project,
        cache: owner.cache,
        name: body.name,
        url: body.url,
        secret: encrypted,
        events: json!(body.events),
        active: body.active,
        created_by: owner.user.id,
        created_at: now,
        updated_at: now,
        last_fired_at: None,
    }
    .into_active_model()
    .insert(&state.web_db)
    .await
    .map_err(|e| WebError::from_db_err(e, "Webhook"))?;

    audit(
        &state,
        &owner,
        Verb::Create,
        &info,
        json!({ "webhook_id": row.id, "name": row.name }),
    )
    .await;
    Ok(ok_json(CreateWebhookResponse {
        webhook: to_response(row),
        secret,
    }))
}

pub async fn read_webhook(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    Ok(ok_json(to_response(owned_webhook(&state, &owner).await?)))
}

pub async fn update_webhook(
    State(state): ApiState,
    info: RequestInfo,
    owner: WebhookOwner,
    Json(body): Json<UpdateWebhookRequest>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let row = owned_webhook(&state, &owner).await?;
    if let Some(url) = &body.url {
        check_url(url)?;
    }
    if let Some(events) = &body.events {
        check_events(events)?;
    }

    let fields: Vec<&str> = [
        body.name.as_ref().map(|_| "name"),
        body.url.as_ref().map(|_| "url"),
        body.events.as_ref().map(|_| "events"),
        body.active.map(|_| "active"),
    ]
    .into_iter()
    .flatten()
    .collect();

    let mut active = row.into_active_model();
    if let Some(name) = body.name {
        active.name = Set(name);
    }
    if let Some(url) = body.url {
        active.url = Set(url);
    }
    if let Some(events) = body.events {
        active.events = Set(json!(events));
    }
    if let Some(enabled) = body.active {
        active.active = Set(enabled);
    }
    active.updated_at = Set(gradient_types::now());
    let row = active
        .update(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Webhook"))?;

    audit(
        &state,
        &owner,
        Verb::Update,
        &info,
        json!({ "webhook_id": row.id, "fields": fields }),
    )
    .await;
    Ok(ok_json(to_response(row)))
}

pub async fn delete_webhook(
    State(state): ApiState,
    info: RequestInfo,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let row = owned_webhook(&state, &owner).await?;
    let (id, name) = (row.id, row.name.clone());
    row.into_active_model().delete(&state.web_db).await?;
    audit(
        &state,
        &owner,
        Verb::Delete,
        &info,
        json!({ "webhook_id": id, "name": name }),
    )
    .await;
    Ok(ok_json(json!({ "deleted": true })))
}

pub async fn rotate_secret(
    State(state): ApiState,
    info: RequestInfo,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let row = owned_webhook(&state, &owner).await?;
    let (secret, encrypted) = new_secret(&state)?;
    let id = row.id;
    let mut active = row.into_active_model();
    active.secret = Set(encrypted);
    active.updated_at = Set(gradient_types::now());
    active.update(&state.web_db).await?;
    audit(
        &state,
        &owner,
        Verb::Update,
        &info,
        json!({ "webhook_id": id, "rotated": true }),
    )
    .await;
    Ok(ok_json(json!({ "secret": secret })))
}

pub async fn test_webhook(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<DeliveryListItem>>> {
    let hook = owned_webhook(&state, &owner).await?;
    let envelope = Envelope::now(webhook::Ping { webhook: hook.id }.into());
    let delivery = gradient_ci::webhooks::deliver(
        &state.ci(),
        &hook,
        &envelope.to_json(),
        PendingDeliveryId::now_v7(),
    )
    .await
    .map_err(|e| WebError::internal(format!("test fire failed: {e:#}")))?;
    Ok(ok_json(to_delivery_item(delivery)))
}

pub async fn list_deliveries(
    State(state): ApiState,
    owner: WebhookOwner,
    Query(q): Query<DeliveryListQuery>,
) -> WebResult<Json<BaseResponse<Vec<DeliveryListItem>>>> {
    let hook = owned_webhook(&state, &owner).await?;
    let rows = EWebhookDelivery::find()
        .filter(CWebhookDelivery::WebhookId.eq(hook.id))
        .order_by(CWebhookDelivery::DeliveredAt, Order::Desc)
        .limit(q.limit.unwrap_or(50).min(200))
        .offset(q.offset.unwrap_or(0))
        .all(&state.web_db)
        .await?;
    Ok(ok_json(rows.into_iter().map(to_delivery_item).collect()))
}

pub async fn get_delivery(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<DeliveryDetail>>> {
    let hook = owned_webhook(&state, &owner).await?;
    let delivery_id: WebhookDeliveryId = owner.param("delivery_id")?;
    let r = EWebhookDelivery::find_by_id(delivery_id)
        .filter(CWebhookDelivery::WebhookId.eq(hook.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Delivery")?;
    Ok(ok_json(DeliveryDetail {
        request_body: r.request_body.clone(),
        response_body: r.response_body.clone(),
        item: to_delivery_item(r),
    }))
}
