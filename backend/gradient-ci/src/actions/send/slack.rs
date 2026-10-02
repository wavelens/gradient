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
use gradient_util::http_validation::validate_webhook_url;
use serde_json::{Value as JsonValue, json};

pub(crate) async fn execute_send_slack_message(
    ctx: &CiContext,
    event: &str,
    envelope: &JsonValue,
    webhook_url: Option<&str>,
) -> Result<ExecutorOk> {
    let plain = decrypt_with_server_key(
        ctx,
        webhook_url.ok_or_else(|| anyhow!("Slack action has no webhook URL"))?,
    )?;
    let webhook = validate_webhook_url(&plain).map_err(|e| anyhow!("webhook URL rejected: {e}"))?;
    let summary = EventSummary::resolve(ctx, event, envelope).await?;
    post_slack_message(&ctx.http, webhook, &summary).await
}

pub(crate) async fn post_slack_message(
    http: &reqwest::Client,
    webhook: reqwest::Url,
    summary: &EventSummary,
) -> Result<ExecutorOk> {
    let resp = http
        .post(webhook)
        .json(&json!({ "text": summary.slack() }))
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .context("Slack send failed")?;
    chat_response("Slack", resp).await
}
