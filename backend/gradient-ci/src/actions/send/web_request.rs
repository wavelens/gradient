/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::actions::crypto::decrypt_action_secret;
use crate::actions::{ExecutorOk, MAX_BODY_BYTES, truncate};
use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_types::input::load_secret_bytes;
use serde_json::Value as JsonValue;

pub(crate) async fn execute_send_web_request(
    ctx: &CiContext,
    event: &str,
    payload: &JsonValue,
    url: &str,
    token: Option<&str>,
) -> Result<ExecutorOk> {
    gradient_util::http_validation::validate_webhook_url(url)
        .map_err(|e| anyhow!("URL rejected: {}", e))?;
    let body = serde_json::to_string(payload).context("serializing webhook payload")?;
    let mut req = ctx
        .http
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Gradient-Event", event)
        .body(body);
    if let Some(tok) = token {
        let key = load_secret_bytes(&ctx.db.config.secrets.crypt_secret_file)
            .context("loading crypt key")?;
        let decrypted = decrypt_action_secret(tok, key.expose())?;
        req = req.bearer_auth(decrypted);
    }
    let resp = req.send().await.context("HTTP send failed")?;
    let status = resp.status().as_u16() as i32;
    let body = resp.text().await.unwrap_or_default();
    Ok(ExecutorOk {
        status_code: Some(status),
        response_body: Some(truncate(body, MAX_BODY_BYTES)),
    })
}
