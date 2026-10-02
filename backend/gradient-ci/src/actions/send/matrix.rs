/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::chat_response;
use crate::actions::ExecutorOk;
use crate::actions::crypto::decrypt_with_server_key;
use crate::actions::summary::EventSummary;
use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_types::TaskActionId;
use gradient_util::http_validation::validate_webhook_url;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};

pub(crate) struct MatrixRoom<'a> {
    pub homeserver: reqwest::Url,
    pub room_id: &'a str,
    pub access_token: &'a str,
}

pub(crate) async fn execute_send_matrix_message(
    ctx: &CiContext,
    action: TaskActionId,
    event: &str,
    envelope: &JsonValue,
    homeserver: &str,
    room_id: &str,
    access_token: Option<&str>,
) -> Result<ExecutorOk> {
    let homeserver =
        validate_webhook_url(homeserver).map_err(|e| anyhow!("homeserver rejected: {e}"))?;
    let access_token = decrypt_with_server_key(
        ctx,
        access_token.ok_or_else(|| anyhow!("Matrix action has no access token"))?,
    )?;
    let summary = EventSummary::resolve(ctx, event, envelope).await?;
    let at = envelope
        .get("at")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let room = MatrixRoom {
        homeserver,
        room_id,
        access_token: &access_token,
    };
    post_matrix_message(
        &ctx.http,
        &room,
        &matrix_txn_id(action, event, at),
        &summary,
    )
    .await
}

pub(crate) async fn post_matrix_message(
    http: &reqwest::Client,
    room: &MatrixRoom<'_>,
    txn_id: &str,
    summary: &EventSummary,
) -> Result<ExecutorOk> {
    let mut url = room.homeserver.clone();
    url.path_segments_mut()
        .map_err(|()| anyhow!("homeserver URL cannot carry a path"))?
        .pop_if_empty()
        .extend([
            "_matrix",
            "client",
            "v3",
            "rooms",
            room.room_id,
            "send",
            "m.room.message",
            txn_id,
        ]);
    let body = json!({
        "msgtype": "m.text",
        "body": summary.plain(),
        "format": "org.matrix.custom.html",
        "formatted_body": summary.html(),
    });
    let resp = http
        .put(url)
        .bearer_auth(room.access_token)
        .json(&body)
        .send()
        .await
        .context("Matrix send failed")?;
    chat_response("Matrix", resp).await
}

pub(crate) fn matrix_txn_id(action: TaskActionId, event: &str, at: &str) -> String {
    let mut hash = Sha256::new();
    for part in [action.to_string().as_str(), event, at] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }

    hex::encode(hash.finalize())
}
