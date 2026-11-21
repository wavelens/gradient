/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::action_result_from_http_response;
use crate::ActionError;
use crate::actions::ExecutorOk;
use crate::actions::crypto::decrypt_with_server_key;
use crate::actions::summary::EventSummary;
use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_util::http::HttpClientForUserSuppliedUrls;
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
    let summary = EventSummary::resolve(ctx, event, envelope).await?;
    post_slack_message(&ctx.db.user_supplied_url_http_client, &plain, &summary).await
}

pub(crate) async fn post_slack_message(
    http: &HttpClientForUserSuppliedUrls,
    webhook: &str,
    summary: &EventSummary,
) -> Result<ExecutorOk> {
    let resp = http
        .request_builder_for_checked_url(reqwest::Method::POST, webhook)
        .map_err(|cause| ActionError::UrlRejected { cause })?
        .json(&json!({ "text": summary.slack() }))
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .context("send to Slack")?;
    action_result_from_http_response("Slack", resp).await
}
