/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::send::{
    execute_exec_tree_pr, execute_git_host_status_report, execute_open_pr, execute_send_mail,
    execute_send_matrix_message, execute_send_slack_message, execute_send_web_request,
    execute_start_evaluation, verify_git_host_action,
};
use super::{MAX_BODY_BYTES, truncate};
use crate::context::CiContext;
use anyhow::{Context, Result};
use gradient_types::{
    ActionConfig, MTaskAction, MTaskActionDelivery, PatchGeneratorKind, TaskActionDeliveryId,
    TaskActionId,
};
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};
use serde_json::Value as JsonValue;
use std::time::Instant;
use tracing::warn;

gradient_db::sql! {
    TOUCH_ACTION_LAST_FIRED = "UPDATE task_action SET last_fired_at = $1, updated_at = $1 \
             WHERE id IN (SELECT id FROM task_action WHERE id = $2 FOR UPDATE SKIP LOCKED)",
        params = [Now, TaskActionId];
}

pub async fn execute_action(
    ctx: &CiContext,
    action: MTaskAction,
    event: &str,
    envelope: JsonValue,
) -> Result<Option<i32>> {
    let cfg: ActionConfig =
        serde_json::from_value(action.config.clone()).context("decode action config")?;
    let action_id = action.id;
    let task_id = action.task;
    let started = Instant::now();
    let request_body = truncate(
        serde_json::to_string(&envelope).unwrap_or_default(),
        MAX_BODY_BYTES,
    );
    let content = envelope.get("content").cloned().unwrap_or(JsonValue::Null);

    let result = match cfg {
        ActionConfig::SendMail {
            recipients,
            subject_template,
        } => {
            execute_send_mail(
                ctx,
                task_id,
                event,
                &envelope,
                &recipients,
                subject_template.as_deref(),
            )
            .await
        }
        ActionConfig::SendWebRequest { url, token } => {
            execute_send_web_request(ctx, event, &envelope, &url, token.as_deref()).await
        }
        ActionConfig::GitHostStatusReport { integration_id } => {
            execute_git_host_status_report(ctx, event, &content, integration_id).await
        }
        ActionConfig::OpenPr {
            integration_id,
            generator: PatchGeneratorKind::ExecTree,
            branch_pattern,
            title_template,
            body_template,
            update_existing,
            ..
        } => {
            execute_exec_tree_pr(
                ctx,
                &content,
                action_id,
                task_id,
                integration_id,
                &branch_pattern,
                title_template.as_deref(),
                body_template.as_deref(),
                update_existing,
            )
            .await
        }
        ActionConfig::OpenPr {
            integration_id,
            branch_pattern,
            title_template,
            body_template,
            update_existing,
            ..
        } => {
            execute_open_pr(
                ctx,
                event,
                &content,
                action_id,
                task_id,
                integration_id,
                &branch_pattern,
                title_template.as_deref(),
                body_template.as_deref(),
                update_existing,
            )
            .await
        }
        ActionConfig::SendMatrixMessage {
            homeserver,
            room_id,
            access_token,
        } => {
            execute_send_matrix_message(
                ctx,
                action_id,
                event,
                &envelope,
                &homeserver,
                &room_id,
                access_token.as_deref(),
            )
            .await
        }
        ActionConfig::SendSlackMessage { webhook_url } => {
            execute_send_slack_message(ctx, event, &envelope, webhook_url.as_deref()).await
        }
        ActionConfig::StartEvaluation { task } => {
            execute_start_evaluation(ctx, &content, task_id, task).await
        }
    };

    let (response_status, response_body, error_message, success) = match &result {
        Ok(ok) => (
            ok.status_code,
            ok.response_body.clone(),
            None,
            ok.status_code.is_none_or(|c| (200..300).contains(&c)),
        ),
        Err(e) => (None, None, Some(format!("{:#}", e)), false),
    };
    record_delivery(
        ctx,
        Attempt {
            action_id,
            event: event.to_string(),
            request_body,
            response_status,
            response_body,
            error_message,
            success,
            started,
        },
    )
    .await;

    // A burst of firings is writing equivalent timestamps to this one row. Skipping a locked
    // row is avoiding a convoy of pool connections behind it.
    let stamp = gradient_types::now();
    let update = TOUCH_ACTION_LAST_FIRED.bind([
        stamp.into(),
        sea_orm::Value::Uuid(Some(action_id.into_inner())),
    ]);
    if let Err(e) = ctx.db.worker_db.execute_raw(update).await {
        warn!(error = %e, %action_id, "Failed to update action last_fired_at");
    }

    result.map(|ok| ok.status_code)
}

const CONNECTIVITY_TEST_EVENT: &str = "test.connectivity";

pub async fn test_git_host_action(
    ctx: &CiContext,
    action: &MTaskAction,
    repository: &str,
) -> Result<()> {
    let started = Instant::now();
    let result = verify_git_host_action(ctx, action, repository).await;
    record_delivery(
        ctx,
        Attempt {
            action_id: action.id,
            event: CONNECTIVITY_TEST_EVENT.to_owned(),
            request_body: String::new(),
            response_status: None,
            response_body: None,
            error_message: result.as_ref().err().map(|e| format!("{e:#}")),
            success: result.is_ok(),
            started,
        },
    )
    .await;
    result
}

struct Attempt {
    action_id: TaskActionId,
    event: String,
    request_body: String,
    response_status: Option<i32>,
    response_body: Option<String>,
    error_message: Option<String>,
    success: bool,
    started: Instant,
}

async fn record_delivery(ctx: &CiContext, attempt: Attempt) {
    let action_id = attempt.action_id;
    let delivery = MTaskActionDelivery {
        id: TaskActionDeliveryId::now_v7(),
        action_id,
        event: attempt.event,
        request_body: attempt.request_body,
        response_status: attempt.response_status,
        response_body: attempt.response_body.map(|s| truncate(s, MAX_BODY_BYTES)),
        error_message: attempt.error_message,
        success: attempt.success,
        duration_ms: i32::try_from(attempt.started.elapsed().as_millis()).unwrap_or(i32::MAX),
        delivered_at: gradient_types::now(),
    }
    .into_active_model();

    if let Err(e) = delivery.insert(&ctx.db.worker_db).await {
        warn!(error = %e, %action_id, "Failed to record action delivery");
    }
}
