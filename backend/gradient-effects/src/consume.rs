/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every consumer is idempotent on its natural key. At-least-once delivery is safe only because of
//! this. A Git host status is keyed per commit and check name, and an open PR is keyed by its
//! branch. A log finalize is replacing the chunk index.

use anyhow::{Context, Result, anyhow};
use gradient_ci::CiContext;
use gradient_ci::actions::{active_actions_for_task, execute_action, matching_actions};
use gradient_ci::pull_request_summary::update_pull_request_summary_on_terminal;
use gradient_ci::reactions::react_to_source_comment_on_terminal;
use gradient_ci::reporting::{CommitStatusCheckWithIds, check_context_kind_for_event};
use gradient_ci::task_commit_status_posting::{
    load_task_git_host_connection,
    post_commit_status_reports_for_task_check_event_and_record_attempt,
    task_should_post_commit_statuses,
};
use gradient_db::deliveries::pending::{
    Outcome, PendingDelivery, PendingDeliveryKind, enqueue, enqueue_or_increment_revision,
};
use gradient_db::deliveries::trigger_fire::TriggerFire;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::events::{Envelope, Event, evaluation};
use gradient_types::ids::{BuildAttemptId, CacheId, TaskActionId, WebhookId};
use gradient_types::*;
use gradient_util::outbound_http_status_error::OutboundHttpNonSuccessStatusError;
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
        PendingDeliveryKind::TriggerFire => fire_trigger(ctx, row).await,
        PendingDeliveryKind::EvalCacheCompact => compact_eval_cache(ctx, row).await,
        PendingDeliveryKind::TaskEventCommitStatusPost => {
            requeue_legacy_task_event_commit_status_post_as_check(ctx, row).await
        }
        PendingDeliveryKind::CommitStatusCheckDelivery => post_commit_status(ctx, row).await,
    };

    match result {
        Ok(()) => Outcome::Delivered,
        Err(e) => failed_delivery_error_to_outcome(&e),
    }
}

fn failed_delivery_error_to_outcome(e: &anyhow::Error) -> Outcome {
    let error = format!("{e:#}");
    let rejection = e
        .chain()
        .find_map(|cause| cause.downcast_ref::<OutboundHttpNonSuccessStatusError>());
    match rejection {
        Some(rejection) if rejection.is_not_retryable() => Outcome::PermanentFailure(error),
        Some(OutboundHttpNonSuccessStatusError {
            retry_after_duration: Some(delay),
            ..
        }) => Outcome::RetryAfterDelay(error, *delay),
        _ => Outcome::Retry(error),
    }
}

async fn compact_eval_cache(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let cache = CacheId::new(uuid_field(row, "cache")?);
    let fingerprint = field(row, "fingerprint")?
        .as_str()
        .ok_or_else(|| anyhow!("pending delivery payload fingerprint is not a string"))?;
    let copies: Vec<CacheId> = serde_json::from_value(field(row, "copies")?.clone())
        .context("decode the caches that hold a copy")?;
    gradient_db::eval_cache::compact(&ctx.db(), cache, fingerprint, &copies).await
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
        serde_json::from_value(row.payload.clone()).context("decode a stored event")?;
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

fn plan_deliveries(
    envelope: &Envelope,
    actions: &[MTaskAction],
    hooks: &[MWebhook],
    commit_status: Option<&(TaskId, CommitStatusCheckWithIds)>,
    parent: &str,
) -> Vec<(PendingDeliveryKind, String, JsonValue)> {
    let name = envelope.event.name();
    let to_commit_status = commit_status.map(|(task, check)| {
        (
            PendingDeliveryKind::CommitStatusCheckDelivery,
            task_commit_status_check_delivery_queue_key(*task, check),
            serde_json::json!({
                "task": task,
                "event": name,
                "envelope": envelope.to_json(),
            }),
        )
    });
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
    to_actions.chain(to_hooks).chain(to_commit_status).collect()
}

/// The whole expansion is one transaction. Either every owed delivery is queued or none is. A
/// retried expansion cannot repeat an external call this way.
async fn fan_out(ctx: &EffectsCtx, envelope: &Envelope, parent: &str) -> Result<()> {
    let ci = ctx.ci();
    let owner = envelope.event.owner();
    let name = envelope.event.name();

    let actions = match owner.task {
        Some(task) => {
            let all = active_actions_for_task(&ci, task)
                .await
                .context("load the task's actions")?;
            matching_actions(all, &name, &envelope.event.content())
        }
        None => Vec::new(),
    };
    let hooks: Vec<MWebhook> = gradient_ci::webhooks::candidates(&ci.db.worker_db, &owner)
        .await
        .context("load the event's webhooks")?
        .into_iter()
        .filter(|h| gradient_ci::webhooks::routes_to(h, &owner, &name, envelope.event.personal()))
        .collect();

    let commit_status = match owner.task {
        Some(task) if check_context_kind_for_event(&name).is_some() => {
            load_task_commit_status_check_for_event_name_and_content(
                &ci,
                task,
                &name,
                &envelope.event.content(),
            )
            .await?
        }
        _ => None,
    };

    let plan = plan_deliveries(envelope, &actions, &hooks, commit_status.as_ref(), parent);
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
        match kind {
            PendingDeliveryKind::CommitStatusCheckDelivery => {
                enqueue_or_increment_revision(&txn, kind, key, payload).await
            }
            _ => enqueue(&txn, kind, key, payload).await,
        }
        .context("enqueue a delivery")?;
    }
    txn.commit()
        .await
        .context("commit the delivery expansion")?;

    Ok(())
}

async fn load_task_commit_status_check_for_event_name_and_content(
    ci: &CiContext,
    task: TaskId,
    event: &str,
    content: &JsonValue,
) -> Result<Option<(TaskId, CommitStatusCheckWithIds)>> {
    let Some((task, _)) = load_task_git_host_connection(ci, task)
        .await
        .context("load the task's git host connection")?
        .filter(|(task, connection)| task_should_post_commit_statuses(task, connection))
    else {
        return Ok(None);
    };
    Ok(
        commit_status_check_updated_by_event_name_and_content(event, content)?
            .map(|check| (task.id, check)),
    )
}

fn commit_status_check_updated_by_event_name_and_content(
    event: &str,
    content: &JsonValue,
) -> Result<Option<CommitStatusCheckWithIds>> {
    if content.get("evaluation_kind").and_then(|v| v.as_str()) == Some("input_update") {
        return Ok(None);
    }
    CommitStatusCheckWithIds::commit_status_check_from_event_name_and_content(event, content)
}

fn task_commit_status_check_delivery_queue_key(
    task: TaskId,
    check: &CommitStatusCheckWithIds,
) -> String {
    match check {
        CommitStatusCheckWithIds::Approval { evaluation } => {
            format!("{task}:approval:{evaluation}")
        }
        CommitStatusCheckWithIds::Evaluation { evaluation } => {
            format!("{task}:evaluation:{evaluation}")
        }
        CommitStatusCheckWithIds::Build { build_job, .. } => format!("{task}:build:{build_job}"),
        CommitStatusCheckWithIds::ExecJob { job, .. } => format!("{task}:exec_job:{job}"),
    }
}

fn read_task_event_and_content_from_payload(
    row: &PendingDelivery,
) -> Result<(TaskId, &str, JsonValue)> {
    let task = TaskId::new(uuid_field(row, "task")?);
    let event = field(row, "event")?
        .as_str()
        .ok_or_else(|| anyhow!("pending delivery payload event is not a string"))?;
    let content = field(row, "envelope")?
        .get("content")
        .cloned()
        .unwrap_or_default();
    Ok((task, event, content))
}

async fn post_commit_status(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let (task, event, content) = read_task_event_and_content_from_payload(row)?;
    let Some(check) = commit_status_check_updated_by_event_name_and_content(event, &content)?
    else {
        return Ok(());
    };
    post_commit_status_reports_for_task_check_event_and_record_attempt(
        &ctx.ci(),
        task,
        event,
        &check,
    )
    .await
}

async fn requeue_legacy_task_event_commit_status_post_as_check(
    ctx: &EffectsCtx,
    row: &PendingDelivery,
) -> Result<()> {
    let (task, event, content) = read_task_event_and_content_from_payload(row)?;
    let Some(check) = commit_status_check_updated_by_event_name_and_content(event, &content)?
    else {
        return Ok(());
    };
    enqueue_or_increment_revision(
        &ctx.db().worker_db,
        PendingDeliveryKind::CommitStatusCheckDelivery,
        task_commit_status_check_delivery_queue_key(task, &check),
        row.payload.clone(),
    )
    .await
    .context("requeue commit status deliveries from an old instance per check")
}

async fn live_webhook<C: ConnectionTrait>(
    db: &C,
    row: &PendingDelivery,
) -> Result<Option<MWebhook>> {
    let id = WebhookId::new(uuid_field(row, "webhook")?);
    let hook = EWebhook::find_by_id(id)
        .one(db)
        .await
        .context("look up the webhook of a delivery")?;
    Ok(hook.filter(|h| h.active))
}

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
            let ci = ctx.ci();
            react_to_source_comment_on_terminal(&ci, task, &evaluation, status).await;
            update_pull_request_summary_on_terminal(&ci, task, &evaluation, status).await;
        }
        Ok(None) => {}
        Err(e) => warn!(error = %e, "looking up the evaluation of a terminal reaction"),
    }
}

async fn finalize(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let attempt = BuildAttemptId::new(uuid_field(row, "attempt")?);

    gradient_db::status::logging::finalize_build_log(&ctx.db(), attempt).await
}

async fn fire_trigger(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    let fire: TriggerFire =
        serde_json::from_value(row.payload.clone()).context("decode a stored trigger fire")?;

    gradient_scheduler::trigger_firing::fire(ctx.scheduler(), &fire).await
}

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
        .context("look up the action of a delivery")?
    else {
        return Ok(());
    };
    if !action.active {
        warn!(%action_id, "skipping a delivery for a deactivated action");
        return Ok(());
    }

    execute_action(&ci, action, &event, envelope)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_db::deliveries::pending::PendingDelivery;
    use gradient_fixtures::mock::mock_db;
    use gradient_types::ids::{BuildJobId, EvaluationId, PendingDeliveryId};

    fn row(payload: JsonValue) -> PendingDelivery {
        PendingDelivery {
            id: PendingDeliveryId::now_v7(),
            kind: PendingDeliveryKind::ActionDelivery,
            key: "k".into(),
            payload,
            attempts: 0,
            revision: 0,
        }
    }

    fn rejection(status: u16, headers: &[(&'static str, &str)]) -> anyhow::Error {
        let mut map = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            map.insert(*name, value.parse().unwrap());
        }
        anyhow::Error::new(
            OutboundHttpNonSuccessStatusError::create_error_from_response_status_and_headers(
                reqwest::StatusCode::from_u16(status).unwrap(),
                &map,
                "rejected".into(),
            ),
        )
        .context("send Git host status report")
    }

    #[test]
    fn a_rejected_request_is_given_up_and_a_rate_limit_waits() {
        assert!(matches!(
            failed_delivery_error_to_outcome(&rejection(422, &[])),
            Outcome::PermanentFailure(_)
        ));
        assert!(matches!(
            failed_delivery_error_to_outcome(&rejection(502, &[])),
            Outcome::Retry(_)
        ));
        assert_eq!(
            failed_delivery_error_to_outcome(&rejection(429, &[("retry-after", "1200")])),
            Outcome::RetryAfterDelay(
                "send Git host status report: rejected".into(),
                std::time::Duration::from_secs(1200)
            )
        );
        assert!(matches!(
            failed_delivery_error_to_outcome(&anyhow!("connection reset")),
            Outcome::Retry(_)
        ));
    }

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
        let db = mock_db()
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
        let db = mock_db()
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
        let commit_status_check_with_task_id =
            commit_status_check_updated_by_event_name_and_content(
                &env.event.name(),
                &env.event.content(),
            )
            .unwrap()
            .map(|check| (TaskId::now_v7(), check));
        let plan = plan_deliveries(
            &env,
            &actions,
            &hooks,
            commit_status_check_with_task_id.as_ref(),
            "parent",
        );
        let kinds: Vec<_> = plan.iter().map(|(kind, _, _)| *kind).collect();
        assert_eq!(
            kinds,
            vec![
                PendingDeliveryKind::ActionDelivery,
                PendingDeliveryKind::WebhookDelivery,
                PendingDeliveryKind::CommitStatusCheckDelivery
            ]
        );
        assert!(
            plan[..2]
                .iter()
                .all(|(_, key, _)| key.ends_with(":build.completed:parent"))
        );
    }

    fn planned_delivery_queue_keys_for_kind(
        plan: &[(PendingDeliveryKind, String, JsonValue)],
        kind: PendingDeliveryKind,
    ) -> Vec<String> {
        plan.iter()
            .filter(|(planned, _, _)| *planned == kind)
            .map(|(_, key, _)| key.clone())
            .collect()
    }

    #[test]
    fn build_queued_and_completed_plan_same_commit_status_queue_key_but_different_action_delivery_keys()
     {
        let task = TaskId::now_v7();
        let actions = vec![MTaskAction {
            id: TaskActionId::now_v7(),
            ..Default::default()
        }];
        let build = gradient_types::events::build::Reported {
            build_id: BuildJobId::now_v7(),
            evaluation_id: EvaluationId::now_v7(),
            ..Default::default()
        };
        let delivery_plan_for_build_status = |status| {
            let env = Envelope::now(
                gradient_types::events::build::Reported {
                    status,
                    ..build.clone()
                }
                .into(),
            );
            let check = commit_status_check_updated_by_event_name_and_content(
                &env.event.name(),
                &env.event.content(),
            )
            .unwrap()
            .map(|check| (task, check));
            plan_deliveries(&env, &actions, &[], check.as_ref(), "parent")
        };

        let queued = delivery_plan_for_build_status(1);
        let completed = delivery_plan_for_build_status(3);

        let status_keys = planned_delivery_queue_keys_for_kind(
            &queued,
            PendingDeliveryKind::CommitStatusCheckDelivery,
        );
        assert_eq!(status_keys.len(), 1);
        assert_eq!(
            status_keys,
            planned_delivery_queue_keys_for_kind(
                &completed,
                PendingDeliveryKind::CommitStatusCheckDelivery
            )
        );
        assert_ne!(
            planned_delivery_queue_keys_for_kind(&queued, PendingDeliveryKind::ActionDelivery),
            planned_delivery_queue_keys_for_kind(&completed, PendingDeliveryKind::ActionDelivery)
        );
    }

    #[test]
    fn evaluation_action_required_and_queued_map_to_checks_but_input_update_maps_to_none() {
        let evaluation_id = EvaluationId::now_v7();
        let build_evaluation_event_and_return_commit_status_check =
            |phase, evaluation_kind: Option<&str>| {
                let env = Envelope::now(
                    evaluation::Reported {
                        evaluation_id,
                        phase,
                        evaluation_kind: evaluation_kind.map(str::to_owned),
                        ..Default::default()
                    }
                    .into(),
                );
                commit_status_check_updated_by_event_name_and_content(
                    &env.event.name(),
                    &env.event.content(),
                )
                .unwrap()
            };

        assert_eq!(
            build_evaluation_event_and_return_commit_status_check(
                evaluation::Phase::ActionRequired,
                None
            ),
            Some(CommitStatusCheckWithIds::Approval {
                evaluation: evaluation_id
            })
        );
        assert_eq!(
            build_evaluation_event_and_return_commit_status_check(evaluation::Phase::Queued, None),
            Some(CommitStatusCheckWithIds::Evaluation {
                evaluation: evaluation_id
            })
        );
        assert_eq!(
            build_evaluation_event_and_return_commit_status_check(
                evaluation::Phase::Queued,
                Some("input_update")
            ),
            None
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

    #[test]
    fn plan_task_commit_status_post_for_report_status_task() {
        let env = Envelope::now(
            evaluation::Reported {
                phase: evaluation::Phase::Completed,
                ..Default::default()
            }
            .into(),
        );
        let build_task_with_report_status = |report_status| MTask {
            report_status,
            ..Default::default()
        };
        let make_git_host_connection_with_reports_status = |reports_status| MGitHostConnection {
            reports_status,
            ..Default::default()
        };

        assert!(task_should_post_commit_statuses(
            &build_task_with_report_status(true),
            &make_git_host_connection_with_reports_status(true)
        ));
        assert!(!task_should_post_commit_statuses(
            &build_task_with_report_status(true),
            &make_git_host_connection_with_reports_status(false)
        ));
        assert!(!task_should_post_commit_statuses(
            &build_task_with_report_status(false),
            &make_git_host_connection_with_reports_status(true)
        ));
        assert!(
            plan_deliveries(&env, &[], &[], None, "parent")
                .iter()
                .all(|(kind, _, _)| *kind != PendingDeliveryKind::CommitStatusCheckDelivery)
        );
    }
}
