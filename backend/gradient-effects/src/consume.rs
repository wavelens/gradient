/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What one pending-delivery row owes. An event expands into the deliveries it implies
//! and writes them back as rows, so an expansion that half-succeeds costs a
//! retry of the expansion and never a duplicated external call; a delivery row
//! is exactly one external call.
//!
//! Every consumer is idempotent on its natural key, which is what makes
//! at-least-once delivery safe: a Git host status is per commit plus check
//! name, an open-PR by its branch, a log finalize replaces the chunk index.

use anyhow::{Context, Result, anyhow};
use gradient_ci::actions::{active_actions_for_task, execute_action, matching_actions};
use gradient_ci::reactions::react_to_source_comment_on_terminal;
use gradient_db::pending_deliveries::{Outcome, PendingDelivery, PendingDeliveryKind, enqueue};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::events::{Envelope, Event, evaluation};
use gradient_types::ids::{BuildAttemptId, TaskActionId, WebhookId};
use gradient_types::*;
use sea_orm::{ConnectionTrait, EntityTrait, TransactionTrait};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::deliver::EffectsCtx;

pub async fn consume(ctx: &EffectsCtx, row: &PendingDelivery) -> Outcome {
    let result = match row.kind {
        PendingDeliveryKind::Event => expand_event(ctx, row).await,
        PendingDeliveryKind::LogFinalize => finalize(ctx, row).await,
        PendingDeliveryKind::ActionDelivery => deliver_action(ctx, row).await,
        PendingDeliveryKind::WebhookDelivery => deliver_webhook(ctx, row).await,
    };

    match result {
        Ok(()) => Outcome::Delivered,
        Err(e) => Outcome::Retry(format!("{e:#}")),
    }
}

fn field<'a>(row: &'a PendingDelivery, name: &str) -> Result<&'a JsonValue> {
    row.payload
        .get(name)
        .ok_or_else(|| anyhow!("pending delivery payload has no {name}"))
}

fn uuid_field(row: &PendingDelivery, name: &str) -> Result<uuid::Uuid> {
    field(row, name)?
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("pending delivery payload {name} is not a uuid"))
}

async fn expand_event(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let event: Event =
        serde_json::from_value(row.payload.clone()).context("decoding a stored event")?;
    let Some(event) = crate::enrich::enrich(&ctx.db(), event).await? else {
        return Ok(());
    };
    let envelope = Envelope::now(event);
    fan_out(ctx, &envelope, &row.key).await?;
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

/// Every delivery row this event owes: one per matching action, one per routed webhook.
fn plan_deliveries(
    envelope: &Envelope,
    actions: &[MTaskAction],
    hooks: &[MWebhook],
    parent: &str,
) -> Vec<(PendingDeliveryKind, String, JsonValue)> {
    let name = envelope.event.name();
    let to_actions = actions.iter().map(|action| {
        (
            PendingDeliveryKind::ActionDelivery,
            format!("{}:{name}:{parent}", action.id),
            action_delivery_payload(action.id, envelope),
        )
    });
    let to_hooks = hooks.iter().map(|hook| {
        (
            PendingDeliveryKind::WebhookDelivery,
            format!("{}:{name}:{parent}", hook.id),
            serde_json::json!({
                "webhook": hook.id,
                "event": name,
                "envelope": envelope.to_json(),
            }),
        )
    });
    to_actions.chain(to_hooks).collect()
}

/// The whole expansion in one transaction: either every delivery this event
/// owes is queued or none is, so a retried expansion never repeats a call.
async fn fan_out(ctx: &EffectsCtx, envelope: &Envelope, parent: &str) -> Result<()> {
    let ci = ctx.ci();
    let owner = envelope.event.owner();
    let name = envelope.event.name();

    let actions = match owner.task {
        Some(task) => {
            let all = active_actions_for_task(&ci, task)
                .await
                .context("loading the task's actions")?;
            matching_actions(all, &name, &envelope.event.content())
        }
        None => Vec::new(),
    };
    let hooks: Vec<MWebhook> = gradient_ci::webhooks::candidates(&ci.db.worker_db, &owner)
        .await
        .context("loading the event's webhooks")?
        .into_iter()
        .filter(|h| gradient_ci::webhooks::routes_to(h, &owner, &name, envelope.event.personal()))
        .collect();

    let plan = plan_deliveries(envelope, &actions, &hooks, parent);
    if plan.is_empty() {
        return Ok(());
    }
    let txn = ci
        .db
        .worker_db
        .begin()
        .await
        .context("begin the delivery expansion")?;
    for (kind, key, payload) in plan {
        enqueue(&txn, kind, key, payload)
            .await
            .context("enqueue a delivery")?;
    }
    txn.commit()
        .await
        .context("commit the delivery expansion")?;

    Ok(())
}

/// A webhook deleted or deactivated while the row waited is delivered-by-omission.
async fn live_webhook<C: ConnectionTrait>(
    db: &C,
    row: &PendingDelivery,
) -> Result<Option<MWebhook>> {
    let id = WebhookId::new(uuid_field(row, "webhook")?);
    let hook = EWebhook::find_by_id(id)
        .one(db)
        .await
        .context("looking up the webhook of a delivery")?;
    Ok(hook.filter(|h| h.active))
}

/// A non-2xx answer is logged on the delivery row and retried with backoff.
async fn deliver_webhook(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let ci = ctx.ci();
    let Some(hook) = live_webhook(&ci.db.worker_db, row).await? else {
        return Ok(());
    };
    let envelope = field(row, "envelope")?;
    let delivery = gradient_ci::webhooks::deliver(&ci, &hook, envelope, row.id).await?;
    if delivery.success {
        return Ok(());
    }
    Err(anyhow!(
        "webhook answered {:?}: {}",
        delivery.response_status,
        delivery.error_message.unwrap_or_default()
    ))
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
async fn finalize(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let attempt = BuildAttemptId::new(uuid_field(row, "attempt")?);

    gradient_db::status::logging::finalize_build_log(&ctx.db(), attempt).await
}

/// One external call. An action deleted or deactivated while the row waited is
/// delivered-by-omission: nothing is owed to a rule that no longer exists.
async fn deliver_action(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let ci = ctx.ci();
    let action_id = TaskActionId::new(uuid_field(row, "action")?);
    let event = field(row, "event")?
        .as_str()
        .ok_or_else(|| anyhow!("pending delivery payload event is not a string"))?
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
    use gradient_db::pending_deliveries::PendingDelivery;
    use gradient_types::ids::PendingDeliveryId;

    fn row(payload: JsonValue) -> PendingDelivery {
        PendingDelivery {
            id: PendingDeliveryId::now_v7(),
            kind: PendingDeliveryKind::ActionDelivery,
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

    #[tokio::test]
    async fn a_delivery_for_a_deleted_webhook_settles() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<MWebhook>::new()])
            .into_connection();
        let r = row(serde_json::json!({
            "webhook": uuid::Uuid::now_v7(),
            "event": "gc.swept",
            "envelope": {},
        }));
        assert!(live_webhook(&db, &r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_delivery_for_a_deactivated_webhook_settles() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![MWebhook {
                active: false,
                ..Default::default()
            }]])
            .into_connection();
        let r = row(serde_json::json!({ "webhook": uuid::Uuid::now_v7() }));
        assert!(live_webhook(&db, &r).await.unwrap().is_none());
    }

    #[test]
    fn one_plan_holds_every_action_and_webhook_delivery() {
        let env = Envelope::now(
            gradient_types::events::build::Reported {
                status: 3,
                ..Default::default()
            }
            .into(),
        );
        let actions = vec![MTaskAction {
            id: TaskActionId::now_v7(),
            ..Default::default()
        }];
        let hooks = vec![MWebhook {
            id: WebhookId::now_v7(),
            ..Default::default()
        }];
        let plan = plan_deliveries(&env, &actions, &hooks, "parent");
        let kinds: Vec<_> = plan.iter().map(|(kind, _, _)| *kind).collect();
        assert_eq!(
            kinds,
            vec![
                PendingDeliveryKind::ActionDelivery,
                PendingDeliveryKind::WebhookDelivery
            ]
        );
        assert!(
            plan.iter()
                .all(|(_, key, _)| key.ends_with(":build.completed:parent"))
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
