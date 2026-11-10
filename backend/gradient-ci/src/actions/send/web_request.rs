/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ActionError;
use crate::actions::ExecutorOk;
use crate::actions::crypto::decrypt_with_server_key;
use crate::actions::send::action_result_from_http_response;
use crate::context::CiContext;
use anyhow::{Context, Result};
use serde_json::Value as JsonValue;

pub(crate) async fn execute_send_web_request(
    ctx: &CiContext,
    event: &str,
    envelope: &JsonValue,
    url: &str,
    token: Option<&str>,
) -> Result<ExecutorOk> {
    gradient_util::http_validation::validate_webhook_url(url)
        .map_err(|cause| ActionError::UrlRejected { cause })?;
    let body = serde_json::to_string(envelope).context("serialize webhook envelope")?;
    let mut req = gradient_util::http::public_client()
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Gradient-Event", event)
        .body(body.clone());
    if let Some(tok) = token {
        let decrypted = decrypt_with_server_key(ctx, tok)?;
        if let Some(signature) = crate::webhooks::sign(decrypted.as_bytes(), body.as_bytes()) {
            req = req.header("X-Gradient-Signature", signature);
        }
    }
    let resp = req.send().await.context("send HTTP request")?;
    action_result_from_http_response("Web request target", resp).await
}
