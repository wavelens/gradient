/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TriggerPostRequestRefusal;
use super::response::TriggerFiringQueuedResponse;
use crate::error::{WebError, WebResult};
use crate::extractors::Path;
use crate::helpers::ok_json;
use anyhow::Context;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use gradient_ci::actions::decrypt_secret_with_file;
use gradient_core::ServerState;
use gradient_db::deliveries::trigger_fire::{
    TriggerFire, enqueue_or_increment_trigger_fire_for_post_request,
};
use gradient_db::lookup::by_id;
use gradient_db::{DbContext, WebDb};
use gradient_git_host::verify_request_against_plaintext_secret;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use std::sync::Arc;
use tracing::warn;

#[utoipa::path(
    post,
    path = "/hooks/trigger/{trigger_id}",
    operation_id = "triggerWebhook",
    tag = "git-host-webhooks",
    summary = "Fire a POST request trigger",
    description = "Fire one trigger of type `post_request`. The trigger secret can prove the origin, so the request needs no login. These headers can carry the secret. A single valid header is enough.\n- `Authorization: Bearer <secret>`\n- `X-Hub-Signature-256` (HMAC-SHA256 of the body, GitHub format)\n- `X-Forgejo-Signature` / `X-Gitea-Signature` (HMAC-SHA256 of the body in hex)\n- `X-Gitlab-Token` (plain secret)\n\nOnly signature checks read the body. Requests with a valid secret receive `202` and queue one firing. All firings check the trigger branch in the same way as a polling trigger. New commits start an evaluation for the firing.",
    params(("trigger_id" = TaskTriggerId, Path)),
    request_body(content = Option<serde_json::Value>, content_type = "application/json"),
    responses((status = ACCEPTED, description = "Gradient queued the trigger.", body = TriggerFiringQueuedResponse), WebError)
)]
pub async fn trigger_webhook(
    State(state): State<Arc<ServerState>>,
    Path(trigger_id): Path<TaskTriggerId>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<(StatusCode, Json<BaseResponse<TriggerFiringQueuedResponse>>)> {
    let (trigger, _task) =
        load_active_post_request_trigger_with_active_task_by_id(&state.web_db, trigger_id).await?;
    let encrypted_secret = trigger
        .encrypted_secret
        .as_deref()
        .ok_or(TriggerPostRequestRefusal::TriggerSecretMissing)?;
    let secret = decrypt_secret_with_file(&state.config.secrets.crypt_file, encrypted_secret)
        .inspect_err(
            |e| warn!(error = %e, trigger = %trigger.id, "Failed to decrypt the trigger secret"),
        )
        .context("internal error")?;
    if secret.expose().is_empty() {
        return Err(TriggerPostRequestRefusal::TriggerSecretMissing.into());
    }
    if !verify_request_against_plaintext_secret(secret.expose(), &headers, &body) {
        warn!(trigger = %trigger.id, "Invalid credentials for the trigger of a post_request");
        return Err(TriggerPostRequestRefusal::MissingAuthenticationCredentials.into());
    }

    enqueue_post_request_trigger_fire_and_wake_delivery_consumer(&state.db(), &trigger).await?;
    Ok((
        StatusCode::ACCEPTED,
        ok_json(TriggerFiringQueuedResponse {
            trigger: trigger.id,
        }),
    ))
}

async fn load_active_post_request_trigger_with_active_task_by_id(
    db: &WebDb,
    id: TaskTriggerId,
) -> WebResult<(MTaskTrigger, MTask)> {
    let trigger = by_id::<ETaskTrigger, _>(db, id)
        .await
        .context("internal error")?
        .filter(|trigger| trigger.active && trigger.trigger_type == TriggerType::PostRequest)
        .ok_or(TriggerPostRequestRefusal::PostRequestTriggerNotFoundOrTaskInactive)?;
    let task = by_id::<ETask, _>(db, trigger.task)
        .await
        .context("internal error")?
        .filter(|task| task.active)
        .ok_or(TriggerPostRequestRefusal::PostRequestTriggerNotFoundOrTaskInactive)?;
    Ok((trigger, task))
}

async fn enqueue_post_request_trigger_fire_and_wake_delivery_consumer(
    db: &DbContext,
    trigger: &MTaskTrigger,
) -> WebResult<()> {
    enqueue_or_increment_trigger_fire_for_post_request(
        &db.worker_db,
        &TriggerFire {
            trigger: trigger.id,
            last_fired_at: trigger.last_fired_at,
        },
    )
    .await
    .context("internal error")?;
    db.wake.delivery.notify_one();
    Ok(())
}
