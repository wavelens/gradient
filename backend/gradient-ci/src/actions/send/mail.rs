/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::actions::ExecutorOk;
use crate::actions::payload::{render_default_body, render_subject};
use crate::context::CiContext;
use anyhow::{Result, anyhow};
use serde_json::Value as JsonValue;

pub(crate) async fn execute_send_mail(
    ctx: &CiContext,
    event: &str,
    payload: &JsonValue,
    recipients: &[String],
    subject_template: Option<&str>,
) -> Result<ExecutorOk> {
    if recipients.is_empty() {
        return Err(anyhow!("send_mail action has no recipients"));
    }
    let subject = render_subject(subject_template, event, payload);
    let body = render_default_body(event, payload);
    let r = ctx
        .email
        .send_action_mail(recipients, &subject, &body)
        .await?;
    Ok(ExecutorOk {
        status_code: Some(r.status_code),
        response_body: Some(r.server_response),
    })
}
