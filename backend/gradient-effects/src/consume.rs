/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What one outbox row owes. An event expands into the deliveries it implies
//! and writes them back as rows, so an expansion that half-succeeds costs a
//! retry of the expansion and never a duplicated external call; a delivery row
//! is exactly one external call.
//!
//! Every consumer is idempotent on its natural key, which is what makes
//! at-least-once delivery safe: a forge status is keyed by commit plus check
//! name, an open-PR by its branch, a log finalize replaces the chunk index.

use anyhow::{Context, Result, anyhow};
use gradient_ci::actions::{active_actions_for_task, execute_action, matching_actions};
use gradient_ci::reactions::react_to_source_comment_on_terminal;
use gradient_db::outbox::{OutboxKind, OutboxRow, Outcome, enqueue};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::events::{Envelope, Event, evaluation};
use gradient_types::ids::{BuildAttemptId, TaskActionId};
use gradient_types::*;
use sea_orm::{EntityTrait, TransactionTrait};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::deliver::EffectsCtx;

pub async fn consume(ctx: &EffectsCtx, row: &OutboxRow) -> Outcome {
    let result = match row.kind {
        OutboxKind::Event => expand_event(ctx, row).await,
        OutboxKind::LogFinalize => finalize(ctx, row).await,
        OutboxKind::ActionDelivery => deliver_action(ctx, row).await,
        OutboxKind::WebhookDelivery => deliver_webhook(ctx, row).await,
    };

    match result {
        Ok(()) => Outcome::Delivered,
        Err(e) => Outcome::Retry(format!("{e:#}")),
    }
}

fn field<'a>(row: &'a OutboxRow, name: &str) -> Result<&'a JsonValue> {
    row.payload
        .get(name)
        .ok_or_else(|| anyhow!("outbox payload has no {name}"))
}

fn uuid_field(row: &OutboxRow, name: &str) -> Result<uuid::Uuid> {
    field(row, name)?
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("outbox payload {name} is not a uuid"))
}

async fn expand_event(ctx: &EffectsCtx, row: &OutboxRow) -> Result<()> {
    let event: Event =
        serde_json::from_value(row.payload.clone()).context("decoding a stored event")?;
    let Some(event) = crate::enrich::enrich(&ctx.db(), event).await? else {
        return Ok(());
    };
    let envelope = Envelope::now(event);
    fan_out_actions(ctx, &envelope, &row.key).await?;
    fan_out_webhooks(ctx, &envelope, &row.key).await?;
    if let Event::EvaluationReported(reported) = &envelope.event {
        react_on_terminal(ctx, reported).await;
    }
    Ok(())
}

pub(crate) fn action_delivery_payload(action: TaskActionId, envelope: &Envelope) -> JsonValue {
    serde_json::json!({
        "action": action,
        "event": envelope.event.name(),
        "envelope": envelope.to_json(),
    })
}

/// One delivery row per matching action, in the transaction that settles the
/// event: either every delivery this event owes is queued or none is, so a
/// crash mid-expansion replays the whole expansion.
async fn fan_out_actions(ctx: &EffectsCtx, envelope: &Envelope, parent: &str) -> Result<()> {
    let Some(task) = envelope.event.owner().task else {
        return Ok(());
    };
    let ci = ctx.ci();
    let actions = active_actions_for_task(&ci, task)
        .await
        .context("loading the task's actions")?;
    let name = envelope.event.name();
    let content = envelope.event.content();

    let txn = ci
        .db
        .worker_db
        .begin()
        .await
        .context("begin the action expansion")?;
    for action in matching_actions(actions, &name, &content) {
        enqueue(
            &txn,
            OutboxKind::ActionDelivery,
            format!("{}:{name}:{parent}", action.id),
            action_delivery_payload(action.id, envelope),
        )
        .await
        .context("enqueue an action delivery")?;
    }
    txn.commit().await.context("commit the action expansion")?;

    Ok(())
}

async fn fan_out_webhooks(_ctx: &EffectsCtx, _envelope: &Envelope, _parent: &str) -> Result<()> {
    Ok(())
}

async fn deliver_webhook(_ctx: &EffectsCtx, _row: &OutboxRow) -> Result<()> {
    Ok(())
}

/// A terminal evaluation reacts on the comment that triggered it; a creation
/// or an approval is not a transition and settles no reaction.
async fn react_on_terminal(ctx: &EffectsCtx, reported: &evaluation::Reported) {
    if reported.created || reported.phase == evaluation::Phase::ApprovalGranted {
        return;
    }
    let (Some(task), Ok(status)) = (
        reported.task,
        EvaluationStatus::try_from(i32::from(reported.status)),
    ) else {
        return;
    };
    match EEvaluation::find_by_id(reported.evaluation_id)
        .one(&ctx.db().worker_db)
        .await
    {
        Ok(Some(evaluation)) => {
            react_to_source_comment_on_terminal(&ctx.ci(), task, &evaluation, status).await;
        }
        Ok(None) => {}
        Err(e) => warn!(error = %e, "looking up the evaluation of a terminal reaction"),
    }
}

/// Compress a finished build's log into chunks. Its own storage failure is a
/// retry, not a lost log: the inline copy stays until the index is written.
async fn finalize(ctx: &EffectsCtx, row: &OutboxRow) -> Result<()> {
    let attempt = BuildAttemptId::new(uuid_field(row, "attempt")?);

    gradient_db::status::logging::finalize_build_log(&ctx.db(), attempt).await
}

/// One external call. An action deleted or deactivated while the row waited is
/// delivered-by-omission: nothing is owed to a rule that no longer exists.
async fn deliver_action(ctx: &EffectsCtx, row: &OutboxRow) -> Result<()> {
    let ci = ctx.ci();
    let action_id = TaskActionId::new(uuid_field(row, "action")?);
    let event = field(row, "event")?
        .as_str()
        .ok_or_else(|| anyhow!("outbox payload event is not a string"))?
        .to_owned();
    let envelope = field(row, "envelope")?.clone();

    let Some(action) = ETaskAction::find_by_id(action_id)
        .one(&ci.db.worker_db)
        .await
        .context("looking up the action of a delivery")?
    else {
        return Ok(());
    };
    if !action.active {
        warn!(%action_id, "skipping a delivery for a deactivated action");
        return Ok(());
    }

    execute_action(&ci, action, &event, envelope).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_db::outbox::OutboxRow;
    use gradient_types::ids::OutboxId;

    fn row(payload: JsonValue) -> OutboxRow {
        OutboxRow {
            id: OutboxId::now_v7(),
            kind: OutboxKind::ActionDelivery,
            key: "k".into(),
            payload,
            attempts: 0,
        }
    }

    /// A payload that lost a field fails the row rather than delivering a
    /// half-built external call; the message names the field.
    #[test]
    fn a_missing_field_names_itself() {
        let r = row(serde_json::json!({"action": "not-a-uuid"}));

        assert!(
            uuid_field(&r, "action")
                .unwrap_err()
                .to_string()
                .contains("not a uuid")
        );
        assert!(
            field(&r, "event")
                .unwrap_err()
                .to_string()
                .contains("no event")
        );
    }

    #[test]
    fn action_delivery_carries_the_envelope() {
        let env = Envelope::now(
            evaluation::Reported {
                phase: evaluation::Phase::Completed,
                ..Default::default()
            }
            .into(),
        );
        let payload = action_delivery_payload(TaskActionId::nil(), &env);
        assert_eq!(payload["event"], "evaluation.completed");
        assert_eq!(payload["envelope"]["event"], "evaluation.completed");
        assert!(payload["envelope"]["content"].is_object());
    }
}
