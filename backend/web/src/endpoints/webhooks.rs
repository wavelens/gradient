/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, OrgAccess, load_org, load_webhook_in_org};
use crate::helpers::ok_json;
use crate::error::{WebError, WebResult};
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};

use gradient_core::ci::{decrypt_webhook_secret, encrypt_webhook_secret, validate_webhook_url};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

fn webhook_org_access() -> OrgAccess {
    OrgAccess::Require {
        permission: Permission::ManageWebhooks,
        reject_managed: false,
    }
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
    let organization = load_org(&state, Caller::User(&user), organization, webhook_org_access()).await?;

    let webhooks = EWebhook::find()
        .filter(CWebhook::Organization.eq(organization.id))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(webhooks.into_iter().map(WebhookResponse::from).collect()))
}

/// `PUT /webhook/{organization}` — create a new webhook.
pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<CreateWebhookRequest>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let organization = load_org(&state, Caller::User(&user), organization, webhook_org_access()).await?;

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
    validate_webhook_url(&body.url).map_err(WebError::BadRequest)?;
    if body.secret.is_empty() {
        return Err(WebError::BadRequest(
            "Webhook secret cannot be empty.".to_string(),
        ));
    }

    let encrypted_secret = encrypt_webhook_secret(&state.config.secrets.crypt_secret_file, &body.secret)
        .map_err(|e| WebError::internal(format!("Failed to encrypt secret: {}", e)))?;

    let webhook = AWebhook {
        id: Set(Uuid::now_v7()),
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
        created_at: Set(gradient_core::types::now()),
    };

    let webhook = webhook.insert(&state.web_db).await?;

    Ok(ok_json(WebhookResponse::from(webhook)))
}

/// `GET /webhook/{organization}/{webhook}` — get a single webhook.
pub async fn get_webhook(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let organization = load_org(&state, Caller::User(&user), organization, webhook_org_access()).await?;
    let webhook = load_webhook_in_org(&state, organization.id, webhook_id).await?;

    Ok(ok_json(WebhookResponse::from(webhook)))
}

/// `PATCH /webhook/{organization}/{webhook}` — update a webhook.
pub async fn patch_webhook(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
    Json(body): Json<UpdateWebhookRequest>,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let organization = load_org(&state, Caller::User(&user), organization, webhook_org_access()).await?;
    let webhook = load_webhook_in_org(&state, organization.id, webhook_id).await?;

    let mut active_webhook: AWebhook = webhook.into_active_model();

    if let Some(name) = body.name {
        active_webhook.name = Set(name);
    }
    if let Some(url) = body.url {
        validate_webhook_url(&url).map_err(WebError::BadRequest)?;
        active_webhook.url = Set(url);
    }
    if let Some(secret) = body.secret {
        let encrypted =
            encrypt_webhook_secret(&state.config.secrets.crypt_secret_file, &secret).map_err(|e| {
                WebError::internal(format!("Failed to encrypt secret: {}", e))
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

    let updated = active_webhook.update(&state.web_db).await?;

    Ok(ok_json(WebhookResponse::from(updated)))
}

/// `DELETE /webhook/{organization}/{webhook}` — delete a webhook.
pub async fn delete_webhook(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let organization = load_org(&state, Caller::User(&user), organization, webhook_org_access()).await?;
    let webhook = load_webhook_in_org(&state, organization.id, webhook_id).await?;

    webhook.into_active_model().delete(&state.web_db).await?;

    Ok(ok_json(true))
}

/// `POST /webhook/{organization}/{webhook}/test` — send a test event.
pub async fn post_webhook_test(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, webhook_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let organization = load_org(&state, Caller::User(&user), organization, webhook_org_access()).await?;
    let webhook = load_webhook_in_org(&state, organization.id, webhook_id).await?;

    let payload = serde_json::json!({
        "event": "ping",
        "data": {
            "test": true,
            "webhook_id": webhook.id,
            "organization": organization.name,
        }
    });

    let body_str = serde_json::to_string(&payload).unwrap_or_default();
    let plaintext_secret = decrypt_webhook_secret(&state.config.secrets.crypt_secret_file, &webhook.secret)
        .map_err(|e| {
            WebError::internal(format!("Failed to decrypt webhook secret: {}", e))
        })?;
    let signature = gradient_core::ci::sign_webhook_payload(plaintext_secret.expose(), &body_str);

    let status = state
        .webhooks
        .deliver(&webhook.url, &signature, "ping", body_str)
        .await
        .map_err(|e| WebError::internal(format!("Webhook delivery failed: {}", e)))?;

    if !(200..300).contains(&status) {
        return Err(WebError::internal(format!(
            "Webhook endpoint returned status {}",
            status
        )));
    }

    Ok(ok_json(true))
}
