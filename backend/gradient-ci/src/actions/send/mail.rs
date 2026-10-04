/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::actions::ExecutorOk;
use crate::actions::summary::EventSummary;
use crate::context::CiContext;
use anyhow::{Result, anyhow};
use gradient_types::{ETask, TaskId};
use sea_orm::EntityTrait;
use serde_json::Value as JsonValue;

pub(crate) async fn execute_send_mail(
    ctx: &CiContext,
    task: TaskId,
    event: &str,
    envelope: &JsonValue,
    recipients: &[String],
    subject_template: Option<&str>,
) -> Result<ExecutorOk> {
    let project = ETask::find_by_id(task)
        .one(&ctx.db.worker_db)
        .await?
        .ok_or_else(|| anyhow!("task {task} of a send_mail action no longer exists"))?
        .project;
    let recipients =
        gradient_db::teams::mail::resolve_mail_recipients(&ctx.db.worker_db, project, recipients)
            .await?;
    if recipients.is_empty() {
        return Err(anyhow!("send_mail action has no recipients"));
    }

    let summary = EventSummary::resolve(ctx, event, envelope).await?;
    let r = ctx
        .email
        .send_action_mail(
            &recipients,
            &summary.subject(subject_template),
            &summary.mail_body(),
        )
        .await?;
    Ok(ExecutorOk {
        status_code: Some(r.status_code),
        response_body: Some(r.server_response),
    })
}
