/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod git_host_status;
mod mail;
pub(crate) mod matrix;
mod open_pr;
pub(crate) mod slack;
mod web_request;

pub(crate) use git_host_status::execute_git_host_status_report;
pub use git_host_status::{reporter_for_task, verify_git_host_action};
pub(crate) use mail::execute_send_mail;
pub(crate) use matrix::execute_send_matrix_message;
pub(crate) use open_pr::execute_open_pr;
pub(crate) use slack::execute_send_slack_message;
pub(crate) use web_request::execute_send_web_request;

use crate::actions::{ExecutorOk, MAX_BODY_BYTES, truncate};
use anyhow::{Result, bail};

pub(super) async fn chat_response(service: &str, resp: reqwest::Response) -> Result<ExecutorOk> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        bail!("{service} returned {status}: {}", truncate(body, 512));
    }

    Ok(ExecutorOk {
        status_code: Some(i32::from(status.as_u16())),
        response_body: Some(truncate(body, MAX_BODY_BYTES)),
    })
}
