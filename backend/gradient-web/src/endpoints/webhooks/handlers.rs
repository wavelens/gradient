/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::WebhookRefusal;
use super::responses::{
    CreateWebhookResponse, DeliveryDetail, DeliveryListItem, RotatedSecret, WebhookDeliveryItem,
    WebhookResponse,
};
use super::scope::{Verb, WebhookOwner};
use crate::audit::Audit;
use crate::endpoints::tasks::DeletedResponse;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::resource::Resource;
use anyhow::Context;
use axum::Json;
use axum::extract::{Query, State};
use gradient_ci::actions::encrypt_action_secret;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_types::events::{Envelope, EventFilter, webhook};
use gradient_types::ids::PendingDeliveryId;
use gradient_types::input::load_secret_bytes;
use gradient_types::*;
use gradient_util::http_validation::validate_webhook_url;
use sea_orm::ActiveValue::Set;
use sea_orm::IntoActiveModel;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

type ApiState = State<Arc<ServerState>>;

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct UpdateWebhookRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WebhookDeliveryListQuery {
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

fn default_true() -> bool {
    true
}

fn check_events(events: &[String]) -> WebResult<()> {
    EventFilter::validate(events).map_err(WebhookRefusal::InvalidEventFilter)?;
    Ok(())
}

fn check_url(url: &str) -> WebResult<()> {
    validate_webhook_url(url).map_err(|e| WebhookRefusal::InvalidUrl(e.to_string()))?;
    Ok(())
}

fn new_secret(config: &RuntimeConfig) -> WebResult<(String, String)> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngExt as _;

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw);
    let plaintext = format!("whs_{}", URL_SAFE_NO_PAD.encode(raw));
    let key = load_secret_bytes(&config.secrets.crypt_file).context("load the encryption key")?;
    let encrypted =
        encrypt_action_secret(&plaintext, key.expose()).context("encrypt the action secret")?;
    Ok((plaintext, encrypted))
}

async fn owned_webhook(db: &WebDb, owner: &WebhookOwner) -> WebResult<MWebhook> {
    let id: WebhookId = owner.param("id")?;
    gradient_db::webhooks::by_id_of_owner(db, owner.into(), id)
        .await?
        .or_not_found(Resource::Webhook)
}

#[utoipa::path(
    get,
    path = "/",
    operation_id = "list{Owner}Webhooks",
    tag = "webhooks",
    summary = "List {owner} webhooks",
    description = "Requires {requires}.",
    responses((status = OK, description = "Webhooks", body = Vec<WebhookResponse>), WebError)
)]
pub async fn list_webhooks(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<Vec<WebhookResponse>>>> {
    let rows = gradient_db::webhooks::of_owner(&state.web_db, (&owner).into()).await?;
    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    post,
    path = "/",
    operation_id = "create{Owner}Webhook",
    tag = "webhooks",
    summary = "Create a {owner} webhook",
    description = "Requires {requires}. The response carries the signing `secret` once; it is\nnever returned again (use `rotate-secret` to replace it).",
    request_body = CreateWebhookRequest,
    responses((status = OK, description = "Created webhook and its secret", body = CreateWebhookResponse), WebError)
)]
pub async fn create_webhook(
    State(state): ApiState,
    audit: Audit,
    owner: WebhookOwner,
    Json(body): Json<CreateWebhookRequest>,
) -> WebResult<Json<BaseResponse<CreateWebhookResponse>>> {
    check_url(&body.url)?;
    check_events(&body.events)?;
    let existing =
        gradient_db::webhooks::by_name_of_owner(&state.web_db, (&owner).into(), &body.name).await?;
    if existing.is_some() {
        return Err(WebError::already_exists(Resource::Webhook));
    }

    let (secret, encrypted) = new_secret(&state.config)?;
    let now = gradient_types::now();
    let row = gradient_db::webhooks::insert(
        &state.web_db,
        MWebhook {
            id: WebhookId::now_v7(),
            scope: owner.scope,
            project: owner.project,
            cache: owner.cache,
            team: owner.team,
            name: body.name,
            url: body.url,
            secret: encrypted,
            events: json!(body.events),
            active: body.active,
            created_by: owner.user.id,
            created_at: now,
            updated_at: now,
            last_fired_at: None,
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::Webhook))?;

    audit
        .write(
            owner.action(Verb::Create),
            owner.event_owner(),
            json!({ "webhook_id": row.id, "name": row.name }),
        )
        .await;
    Ok(ok_json(CreateWebhookResponse {
        webhook: row.into(),
        secret,
    }))
}

#[utoipa::path(
    get,
    path = "/{id}",
    operation_id = "read{Owner}Webhook",
    tag = "webhooks",
    summary = "Read a {owner} webhook",
    params(("id" = WebhookId, Path)),
    responses((status = OK, description = "Webhook", body = WebhookResponse), WebError)
)]
pub async fn read_webhook(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    Ok(ok_json(WebhookResponse::from(
        owned_webhook(&state.web_db, &owner).await?,
    )))
}

#[utoipa::path(
    patch,
    path = "/{id}",
    operation_id = "update{Owner}Webhook",
    tag = "webhooks",
    summary = "Update a {owner} webhook",
    params(("id" = WebhookId, Path)),
    request_body = UpdateWebhookRequest,
    responses((status = OK, description = "Updated webhook", body = WebhookResponse), WebError)
)]
pub async fn update_webhook(
    State(state): ApiState,
    audit: Audit,
    owner: WebhookOwner,
    Json(body): Json<UpdateWebhookRequest>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let row = owned_webhook(&state.web_db, &owner).await?;
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
    let row = gradient_db::webhooks::update(&state.web_db, active)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::Webhook))?;

    audit
        .write(
            owner.action(Verb::Update),
            owner.event_owner(),
            json!({ "webhook_id": row.id, "fields": fields }),
        )
        .await;
    Ok(ok_json(WebhookResponse::from(row)))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    operation_id = "delete{Owner}Webhook",
    tag = "webhooks",
    summary = "Delete a {owner} webhook",
    params(("id" = WebhookId, Path)),
    responses((status = OK, description = "Deleted", body = DeletedResponse), WebError)
)]
pub async fn delete_webhook(
    State(state): ApiState,
    audit: Audit,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let row = owned_webhook(&state.web_db, &owner).await?;
    let (id, name) = (row.id, row.name.clone());
    gradient_db::webhooks::delete(&state.web_db, row).await?;
    audit
        .write(
            owner.action(Verb::Delete),
            owner.event_owner(),
            json!({ "webhook_id": id, "name": name }),
        )
        .await;
    Ok(ok_json(DeletedResponse::deleted()))
}

#[utoipa::path(
    post,
    path = "/{id}/rotate-secret",
    operation_id = "rotate{Owner}WebhookSecret",
    tag = "webhooks",
    summary = "Replace a {owner} webhook's signing secret",
    params(("id" = WebhookId, Path)),
    responses((status = OK, description = "The new secret, shown once", body = RotatedSecret), WebError)
)]
pub async fn rotate_secret(
    State(state): ApiState,
    audit: Audit,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<RotatedSecret>>> {
    let row = owned_webhook(&state.web_db, &owner).await?;
    let (secret, encrypted) = new_secret(&state.config)?;
    let id = row.id;
    let mut active = row.into_active_model();
    active.secret = Set(encrypted);
    active.updated_at = Set(gradient_types::now());
    gradient_db::webhooks::update(&state.web_db, active).await?;
    audit
        .write(
            owner.action(Verb::Update),
            owner.event_owner(),
            json!({ "webhook_id": id, "rotated": true }),
        )
        .await;
    Ok(ok_json(RotatedSecret { secret }))
}

#[utoipa::path(
    post,
    path = "/{id}/test",
    operation_id = "test{Owner}Webhook",
    tag = "webhooks",
    summary = "Send a `webhook.ping` to a {owner} webhook",
    description = "Delivers immediately (no retry) and returns the recorded delivery.",
    params(("id" = WebhookId, Path)),
    responses((status = OK, description = "Recorded delivery", body = DeliveryListItem<WebhookDeliveryId>), WebError)
)]
pub async fn test_webhook(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<DeliveryListItem<WebhookDeliveryId>>>> {
    let hook = owned_webhook(&state.web_db, &owner).await?;
    let envelope = Envelope::now(webhook::Ping { webhook: hook.id }.into());
    let delivery = gradient_ci::webhooks::deliver(
        &state.ci(),
        &hook,
        &envelope.to_json(),
        PendingDeliveryId::now_v7(),
    )
    .await
    .context("run the test fire")?;
    Ok(ok_json(DeliveryListItem::from(delivery)))
}

#[utoipa::path(
    get,
    path = "/{id}/deliveries",
    operation_id = "list{Owner}WebhookDeliveries",
    tag = "webhooks",
    summary = "List a {owner} webhook's deliveries",
    params(WebhookDeliveryListQuery, ("id" = WebhookId, Path)),
    responses((status = OK, description = "Deliveries, newest first", body = Vec<WebhookDeliveryItem>), WebError)
)]
pub async fn list_deliveries(
    State(state): ApiState,
    owner: WebhookOwner,
    Query(q): Query<WebhookDeliveryListQuery>,
) -> WebResult<Json<BaseResponse<Vec<DeliveryListItem<WebhookDeliveryId>>>>> {
    let hook = owned_webhook(&state.web_db, &owner).await?;
    let rows = gradient_db::webhooks::deliveries_of(
        &state.web_db,
        hook.id,
        q.limit.unwrap_or(50).min(200),
        q.offset.unwrap_or(0),
    )
    .await?;
    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/{id}/deliveries/{delivery_id}",
    operation_id = "get{Owner}WebhookDelivery",
    tag = "webhooks",
    summary = "Read one {owner} webhook delivery with bodies",
    params(("id" = WebhookId, Path), ("delivery_id" = WebhookDeliveryId, Path)),
    responses((status = OK, description = "Delivery", body = DeliveryDetail<WebhookDeliveryId>), WebError)
)]
pub async fn get_delivery(
    State(state): ApiState,
    owner: WebhookOwner,
) -> WebResult<Json<BaseResponse<DeliveryDetail<WebhookDeliveryId>>>> {
    let hook = owned_webhook(&state.web_db, &owner).await?;
    let delivery_id: WebhookDeliveryId = owner.param("delivery_id")?;
    let r = gradient_db::webhooks::delivery_of(&state.web_db, hook.id, delivery_id)
        .await?
        .or_not_found(Resource::Delivery)?;
    Ok(ok_json(r.into()))
}
