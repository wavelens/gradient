/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::Utc;
use core::ci::{decrypt_webhook_secret, encrypt_webhook_secret};
use core::db::get_any_organization_by_name;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use super::projects::user_can_edit;

// ── Access helpers ────────────────────────────────────────────────────────────

/// Load an organization by name and verify the user has Admin/Write permission.
async fn load_webhook_org(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
) -> WebResult<MOrganization> {
    let organization = get_any_organization_by_name(Arc::clone(state), org_name)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    if !user_can_edit(state, user_id, organization.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to manage webhooks for this organization.".to_string(),
        ));
    }

    Ok(organization)
}

/// Load a webhook by UUID, scoped to the given organization.
async fn load_webhook(
    state: &Arc<ServerState>,
    org_id: Uuid,
    webhook_id: Uuid,
) -> WebResult<MWebhook> {
    EWebhook::find()
        .filter(CWebhook::Id.eq(webhook_id))
        .filter(CWebhook::Organization.eq(org_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Webhook"))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub url: String,
    pub secret: String,
    pub events: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UpdateWebhookRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub secret: Option<String>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

/// Public-safe webhook view — secret is never exposed.
#[derive(Serialize, Debug)]
pub struct WebhookResponse {
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub url: String,
    pub events: serde_json::Value,
    pub active: bool,
    pub created_by: Uuid,
    pub created_at: chrono::NaiveDateTime,
}

impl From<MWebhook> for WebhookResponse {
    fn from(w: MWebhook) -> Self {
        WebhookResponse {
            id: w.id,
            organization: w.organization,
            name: w.name,
            url: w.url,
            events: w.events,
            active: w.active,
            created_by: w.created_by,
            created_at: w.created_at,
        }
    }
}

/// `GET /webhook/{organization}` — list all webhooks for an organization.
pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<WebhookResponse>>>> {
    let organization = load_webhook_org(&state, user.id, organization).await?;

    let webhooks = EWebhook::find()
        .filter(CWebhook::Organization.eq(organization.id))
        .all(&state.db)
        .await?;

    Ok(Json(BaseResponse {
        error: false,
        message: webhooks.into_iter().map(WebhookResponse::from).collect(),
    }))
}

/// `PUT /webhook/{organization}` — create a new webhook.
pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<CreateWebhookRequest>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let organization = load_webhook_org(&state, user.id, organization).await?;

    if body.name.is_empty() {
        return Err(WebError::BadRequest(
            "Webhook name cannot be empty.".to_string(),
        ));
    }
    if body.url.is_empty() {
        return Err(WebError::BadRequest(
            "Webhook URL cannot be empty.".to_string(),
        ));
    }
    if body.secret.is_empty() {
        return Err(WebError::BadRequest(
            "Webhook secret cannot be empty.".to_string(),
        ));
    }

    let encrypted_secret = encrypt_webhook_secret(&state.cli.crypt_secret_file, &body.secret)
        .map_err(|e| WebError::InternalServerError(format!("Failed to encrypt secret: {}", e)))?;

    let webhook = AWebhook {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        name: Set(body.name),
        url: Set(body.url),
        secret: Set(encrypted_secret),
        events: Set(serde_json::Value::Array(
            body.events
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        )),
        active: Set(true),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let webhook = webhook.insert(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: WebhookResponse::from(webhook),
    }))
}

/// `GET /webhook/{organization}/{webhook}` — get a single webhook.
pub async fn get_webhook(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let organization = load_webhook_org(&state, user.id, organization).await?;
    let webhook = load_webhook(&state, organization.id, webhook_id).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: WebhookResponse::from(webhook),
    }))
}

/// `PATCH /webhook/{organization}/{webhook}` — update a webhook.
pub async fn patch_webhook(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
    Json(body): Json<UpdateWebhookRequest>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let organization = load_webhook_org(&state, user.id, organization).await?;
    let webhook = load_webhook(&state, organization.id, webhook_id).await?;

    let mut active_webhook: AWebhook = webhook.into_active_model();

    if let Some(name) = body.name {
        active_webhook.name = Set(name);
    }
    if let Some(url) = body.url {
        active_webhook.url = Set(url);
    }
    if let Some(secret) = body.secret {
        let encrypted =
            encrypt_webhook_secret(&state.cli.crypt_secret_file, &secret).map_err(|e| {
                WebError::InternalServerError(format!("Failed to encrypt secret: {}", e))
            })?;
        active_webhook.secret = Set(encrypted);
    }
    if let Some(events) = body.events {
        active_webhook.events = Set(serde_json::Value::Array(
            events.into_iter().map(serde_json::Value::String).collect(),
        ));
    }
    if let Some(active) = body.active {
        active_webhook.active = Set(active);
    }

    let updated = active_webhook.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: WebhookResponse::from(updated),
    }))
}

/// `DELETE /webhook/{organization}/{webhook}` — delete a webhook.
pub async fn delete_webhook(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let organization = load_webhook_org(&state, user.id, organization).await?;
    let webhook = load_webhook(&state, organization.id, webhook_id).await?;

    webhook.into_active_model().delete(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: true,
    }))
}

/// `POST /webhook/{organization}/{webhook}/test` — send a test event.
pub async fn post_webhook_test(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let organization = load_webhook_org(&state, user.id, organization).await?;
    let webhook = load_webhook(&state, organization.id, webhook_id).await?;

    let payload = serde_json::json!({
        "event": "ping",
        "data": {
            "test": true,
            "webhook_id": webhook.id,
            "organization": organization.name,
        }
    });

    let body_str = serde_json::to_string(&payload).unwrap_or_default();
    let plaintext_secret = decrypt_webhook_secret(&state.cli.crypt_secret_file, &webhook.secret)
        .map_err(|e| {
            WebError::InternalServerError(format!("Failed to decrypt webhook secret: {}", e))
        })?;
    let signature = core::ci::sign_webhook_payload(plaintext_secret.expose(), &body_str);

    let status = state
        .webhooks
        .deliver(&webhook.url, &signature, "ping", body_str)
        .await
        .map_err(|e| WebError::InternalServerError(format!("Webhook delivery failed: {}", e)))?;

    if !(200..300).contains(&status) {
        return Err(WebError::InternalServerError(format!(
            "Webhook endpoint returned status {}",
            status
        )));
    }

    Ok(Json(BaseResponse {
        error: false,
        message: true,
    }))
}
