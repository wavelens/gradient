/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::sign;
use crate::actions::{MAX_BODY_BYTES, decrypt_secret_with_file, truncate};
use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_types::ids::PendingDeliveryId;
use gradient_types::*;
use gradient_util::http_validation::validate_webhook_url;
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};
use serde_json::Value as JsonValue;
use std::time::Instant;
use tracing::warn;

gradient_db::sql! {
    TOUCH_WEBHOOK_LAST_FIRED = "UPDATE webhook SET last_fired_at = $1, updated_at = $1 \
             WHERE id IN (SELECT id FROM webhook WHERE id = $2 FOR UPDATE SKIP LOCKED)",
        params = [Now, NewUuid];
}

/// Transport errors are recorded on the delivery row, not returned.
pub async fn deliver(
    ctx: &CiContext,
    hook: &MWebhook,
    envelope: &JsonValue,
    delivery: PendingDeliveryId,
) -> Result<MWebhookDelivery> {
    let event = envelope
        .get("event")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let body = serde_json::to_string(envelope).context("serializing webhook envelope")?;
    let started = Instant::now();
    let result = post(ctx, hook, &event, &body, delivery).await;
    let duration_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    let (response_status, response_body, error_message) = match result {
        Ok((status, text)) => (Some(status), Some(truncate(text, MAX_BODY_BYTES)), None),
        Err(e) => (None, None, Some(format!("{e:#}"))),
    };
    let success = response_status.is_some_and(|s| (200..300).contains(&s));

    let saved = MWebhookDelivery {
        id: WebhookDeliveryId::now_v7(),
        webhook_id: hook.id,
        event,
        request_body: truncate(body, MAX_BODY_BYTES),
        response_status,
        response_body,
        error_message,
        success,
        duration_ms,
        delivered_at: gradient_types::now(),
    }
    .into_active_model()
    .insert(&ctx.db.worker_db)
    .await
    .context("recording a webhook delivery")?;

    if success {
        touch_last_fired(ctx, hook.id).await;
    }
    Ok(saved)
}

async fn post(
    ctx: &CiContext,
    hook: &MWebhook,
    event: &str,
    body: &str,
    delivery: PendingDeliveryId,
) -> Result<(i32, String)> {
    validate_webhook_url(&hook.url).map_err(|e| anyhow!("URL rejected: {e}"))?;
    let secret = decrypt_secret_with_file(&ctx.db.config.secrets.crypt_file, &hook.secret)?;
    let signature = sign(secret.expose().as_bytes(), body.as_bytes())
        .ok_or_else(|| anyhow!("webhook secret cannot key an HMAC"))?;
    let resp = ctx
        .http
        .post(&hook.url)
        .header("Content-Type", "application/json")
        .header("X-Gradient-Event", event)
        .header("X-Gradient-Delivery", delivery.to_string())
        .header("X-Gradient-Signature", signature)
        .body(body.to_owned())
        .send()
        .await
        .context("HTTP send failed")?;
    let status = i32::from(resp.status().as_u16());
    Ok((status, resp.text().await.unwrap_or_default()))
}

async fn touch_last_fired(ctx: &CiContext, id: WebhookId) {
    let update = TOUCH_WEBHOOK_LAST_FIRED.bind([
        gradient_types::now().into(),
        sea_orm::Value::Uuid(Some(id.into_inner())),
    ]);
    if let Err(e) = ctx.db.worker_db.execute_raw(update).await {
        warn!(error = %e, webhook = %id, "failed to update webhook last_fired_at");
    }
}
