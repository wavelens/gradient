/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What one outbox row owes. The three event kinds expand into the deliveries
//! they imply and write them back as rows, so an expansion that half-succeeds
//! costs a retry of the expansion and never a duplicated external call; an
//! `ActionDelivery` row is exactly one external call.
//!
//! Every consumer is idempotent on its natural key, which is what makes
//! at-least-once delivery safe: a forge status is keyed by commit plus check
//! name, an open-PR by its branch, a log finalize replaces the chunk index.

use anyhow::{Context, Result, anyhow};
use gradient_ci::actions::{active_actions_for_task, execute_action, matching_actions};
use gradient_ci::reactions::react_to_source_comment_on_terminal;
use gradient_ci::reporting::{
    build_event_for_status, eval_kind_str, evaluation_created_event, evaluation_event_for_status,
};
use gradient_db::outbox::{OutboxKind, OutboxRow, Outcome, enqueue};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::ids::{BuildAttemptId, BuildJobId, EvaluationId, TaskActionId};
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::{EntityTrait, TransactionTrait};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::deliver::EffectsCtx;

pub async fn consume(ctx: &EffectsCtx, row: &OutboxRow) -> Outcome {
    let result = match row.kind {
        OutboxKind::BuildStatus => expand_build_status(ctx, row).await,
        OutboxKind::EvaluationStatus => expand_evaluation_status(ctx, row).await,
        OutboxKind::LogFinalize => finalize(ctx, row).await,
        OutboxKind::ActionDelivery => deliver_action(ctx, row).await,
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

fn i32_field(row: &OutboxRow, name: &str) -> Result<i32> {
    field(row, name)?
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| anyhow!("outbox payload {name} is not an i32"))
}

/// An entry point's build reached a status the forges report. Expands into one
/// delivery per matching action, with the same payload the status reactor built.
async fn expand_build_status(ctx: &EffectsCtx, row: &OutboxRow) -> Result<()> {
    let db = ctx.db();
    let status = BuildStatus::try_from(i32_field(row, "status")?)
        .map_err(|_| anyhow!("outbox build status out of range"))?;
    let Some(event) = build_event_for_status(status) else {
        return Ok(());
    };

    let evaluation_id = EvaluationId::new(uuid_field(row, "evaluation")?);
    let Some(evaluation) = EEvaluation::find_by_id(evaluation_id)
        .one(&db.worker_db)
        .await
        .context("looking up the evaluation of a build report")?
    else {
        // Its evaluation was collected while the row waited; nobody is left to
        // report to, and the row is settled rather than retried to the cap.
        return Ok(());
    };
    let Some(task) = evaluation.task else {
        return Ok(());
    };

    let derivation = DerivationId::new(uuid_field(row, "derivation")?);
    let derivation_path = EDerivation::find_by_id(derivation)
        .one(&db.worker_db)
        .await
        .context("looking up the derivation of a build report")?
        .map(|d| d.store_path());

    let payload = serde_json::json!({
        "build_id": BuildJobId::new(uuid_field(row, "build_job")?),
        "evaluation_id": evaluation_id,
        "derivation_path": derivation_path,
        "status": event,
        "evaluation_kind": eval_kind_str(evaluation.kind),
    });

    fan_out(ctx, task, event, &payload, &row.key).await
}

/// An evaluation changed status, or was just created. Both expand the same way;
/// a creation carries the description its first forge check shows.
async fn expand_evaluation_status(ctx: &EffectsCtx, row: &OutboxRow) -> Result<()> {
    let db = ctx.db();
    let evaluation_id = EvaluationId::new(uuid_field(row, "evaluation")?);
    let Some(evaluation) = EEvaluation::find_by_id(evaluation_id)
        .one(&db.worker_db)
        .await
        .context("looking up the evaluation of a status report")?
    else {
        return Ok(());
    };
    let Some(task) = evaluation.task else {
        return Ok(());
    };

    // An event named outright is one no status maps to (the approval gate
    // clearing), so it is reported as written and settles no reaction.
    if let Some(event) = row.payload.get("event").and_then(JsonValue::as_str) {
        let payload = serde_json::json!({
            "evaluation_id": evaluation_id,
            "task_id": task,
            "status": event,
            "evaluation_kind": eval_kind_str(evaluation.kind),
        });

        return fan_out(ctx, task, event, &payload, &row.key).await;
    }

    let status = EvaluationStatus::try_from(i32_field(row, "status")?)
        .map_err(|_| anyhow!("outbox evaluation status out of range"))?;
    let created = row
        .payload
        .get("created")
        .and_then(JsonValue::as_bool)
        .unwrap_or_default();

    let (event, description) = if created {
        let reason = evaluation
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json);
        match evaluation_created_event(status, reason) {
            Some(pair) => pair,
            None => return Ok(()),
        }
    } else {
        (evaluation_event_for_status(status), None)
    };

    let mut payload = serde_json::json!({
        "evaluation_id": evaluation_id,
        "task_id": task,
        "repository": evaluation.repository,
        "status": event,
        "evaluation_kind": eval_kind_str(evaluation.kind),
    });
    if let Some(text) = description {
        payload["description"] = JsonValue::String(text.to_owned());
    }

    fan_out(ctx, task, event, &payload, &row.key).await?;

    if !created {
        react_to_source_comment_on_terminal(&ctx.ci(), task, &evaluation, status).await;
    }

    Ok(())
}

/// One delivery row per matching action, in the transaction that settles the
/// event: either every delivery this event owes is queued or none is, so a
/// crash mid-expansion replays the whole expansion.
async fn fan_out(
    ctx: &EffectsCtx,
    task: TaskId,
    event: &str,
    payload: &JsonValue,
    parent: &str,
) -> Result<()> {
    let ci = ctx.ci();
    let actions = active_actions_for_task(&ci, task)
        .await
        .context("loading the task's actions")?;

    let txn = ci
        .db
        .worker_db
        .begin()
        .await
        .context("begin the delivery expansion")?;
    for action in matching_actions(actions, event, payload) {
        enqueue(
            &txn,
            OutboxKind::ActionDelivery,
            format!("{}:{event}:{parent}", action.id),
            serde_json::json!({
                "action": action.id,
                "event": event,
                "payload": payload,
            }),
        )
        .await
        .context("enqueue an action delivery")?;
    }
    txn.commit()
        .await
        .context("commit the delivery expansion")?;

    Ok(())
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
    let payload = field(row, "payload")?.clone();

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

    execute_action(&ci, action, &event, payload).await
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

    /// Statuses ride as their integer discriminant, so the consumer must read
    /// back exactly what the emitter wrote.
    #[test]
    fn a_status_round_trips_through_its_discriminant() {
        let r = row(serde_json::json!({"status": i32::from(BuildStatus::Completed)}));

        assert_eq!(
            BuildStatus::try_from(i32_field(&r, "status").unwrap()).unwrap(),
            BuildStatus::Completed
        );
    }
}
