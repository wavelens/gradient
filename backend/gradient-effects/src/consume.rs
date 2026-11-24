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
    Outcome, PendingDelivery, PendingDeliveryKind, enqueue_many, enqueue_or_increment_revision,
    enqueue_or_increment_revision_many,
};
use gradient_db::deliveries::trigger_fire::TriggerFire;
use gradient_db::eval_cache::DecodedCompactionJob;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::events::{Envelope, Event, EventOwner, evaluation};
use gradient_types::ids::{BuildAttemptId, PendingDeliveryId, TaskActionId, WebhookId};
use gradient_types::*;
use gradient_util::outbound_http_status_error::OutboundHttpNonSuccessStatusError;
use sea_orm::{ConnectionTrait, DatabaseTransaction, EntityTrait, TransactionTrait};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use tracing::warn;

use crate::deliver::EffectsCtx;

#[derive(Debug, PartialEq)]
pub enum ConsumedDeliveryWork {
    SinglePendingDelivery(PendingDelivery),
    DatabaseOnlyEventExpansionBatch(Vec<PendingDelivery>),
}

impl ConsumedDeliveryWork {
    pub fn first_pending_delivery_id(&self) -> PendingDeliveryId {
        match self {
            Self::SinglePendingDelivery(row) => row.id,
            Self::DatabaseOnlyEventExpansionBatch(rows) => rows[0].id,
        }
    }
}

pub fn split_claimed_rows_into_delivery_jobs(
    rows: Vec<PendingDelivery>,
) -> Vec<ConsumedDeliveryWork> {
    let (mut batch, singles): (Vec<_>, Vec<_>) = rows.into_iter().partition(is_database_only_event);
    let batch = match batch.len() {
        0 => None,
        1 => batch.pop().map(ConsumedDeliveryWork::SinglePendingDelivery),
        _ => Some(ConsumedDeliveryWork::DatabaseOnlyEventExpansionBatch(batch)),
    };
    singles
        .into_iter()
        .map(ConsumedDeliveryWork::SinglePendingDelivery)
        .chain(batch)
        .collect()
}

fn is_database_only_event(row: &PendingDelivery) -> bool {
    row.kind == PendingDeliveryKind::Event
        && decode_event_from_delivery_payload(row)
            .is_ok_and(|event| !matches!(event, Event::EvaluationReported(_)))
}

pub async fn consume_delivery_job_with_row_outcomes(
    ctx: &EffectsCtx,
    job: ConsumedDeliveryWork,
) -> Vec<(PendingDelivery, Outcome)> {
    match job {
        ConsumedDeliveryWork::SinglePendingDelivery(row) => {
            vec![consume_pending_delivery_row_and_pair_with_outcome(ctx, row).await]
        }
        ConsumedDeliveryWork::DatabaseOnlyEventExpansionBatch(rows) => {
            expand_event_rows_with_delivery_outcomes(ctx, rows).await
        }
    }
}

async fn expand_event_rows_with_delivery_outcomes(
    ctx: &EffectsCtx,
    rows: Vec<PendingDelivery>,
) -> Vec<(PendingDelivery, Outcome)> {
    let error = match enrich_events_plan_enqueue_and_run_reactions(ctx, &rows).await {
        Ok(()) => {
            return rows
                .into_iter()
                .map(|row| (row, Outcome::Delivered))
                .collect();
        }
        Err(e) => e,
    };
    warn!(
        error = %format!("{error:#}"),
        rows = rows.len(),
        "Event set expansion failed. Consume each event one by one so only failed events retry."
    );
    let mut finished = Vec::with_capacity(rows.len());
    for row in rows {
        finished.push(consume_pending_delivery_row_and_pair_with_outcome(ctx, row).await);
    }
    finished
}

async fn consume_pending_delivery_row_and_pair_with_outcome(
    ctx: &EffectsCtx,
    row: PendingDelivery,
) -> (PendingDelivery, Outcome) {
    let outcome = match consume(ctx, &row).await {
        Ok(()) => Outcome::Delivered,
        Err(e) => failed_delivery_error_to_outcome(&e),
    };
    (row, outcome)
}

fn decode_event_from_delivery_payload(row: &PendingDelivery) -> Result<Event> {
    serde_json::from_value(row.payload.clone()).context("decode a stored event")
}

async fn consume(ctx: &EffectsCtx, row: &PendingDelivery) -> Result<()> {
    match row.kind {
        PendingDeliveryKind::Event => {
            enrich_events_plan_enqueue_and_run_reactions(ctx, std::slice::from_ref(row)).await
        }
        PendingDeliveryKind::LogFinalize => finalize(ctx, row).await,
        PendingDeliveryKind::ActionDelivery => deliver_action(ctx, row).await,
        PendingDeliveryKind::WebhookDelivery => deliver_webhook(ctx, row).await,
        PendingDeliveryKind::TriggerFire => fire_trigger(ctx, row).await,
        PendingDeliveryKind::EvalCacheCompact => compact_eval_cache(ctx, row).await,
        PendingDeliveryKind::TaskEventCommitStatusPost => {
            requeue_legacy_task_event_commit_status_post_as_check(ctx, row).await
        }
        PendingDeliveryKind::CommitStatusCheckDelivery => post_commit_status(ctx, row).await,
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
    match decode_queued_eval_cache_compaction_from_delivery_payload(row)? {
        Some(queued) => {
            gradient_db::eval_cache::compact(&ctx.db(), &queued.store, &queued.copies).await
        }
        None => Ok(()),
    }
}

fn decode_queued_eval_cache_compaction_from_delivery_payload(
    row: &PendingDelivery,
) -> Result<Option<DecodedCompactionJob>> {
    let queued = gradient_db::eval_cache::decode_queued_compaction_payload(&row.payload)?;
    if queued.is_none() {
        warn!(key = %row.key, "Discard compaction of the eval cache with no project.");
    }
    Ok(queued)
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

#[derive(Default)]
struct EventSubscriberCache {
    cached_active_task_actions_by_task_id: HashMap<TaskId, Vec<MTaskAction>>,
    event_owners_with_webhooks: Vec<(EventOwner, Vec<MWebhook>)>,
    task_id_to_reports_commit_status: HashMap<TaskId, bool>,
}

impl EventSubscriberCache {
    async fn compute_event_deliveries_using_subscriber_cache(
        &mut self,
        ci: &CiContext,
        envelope: &Envelope,
        parent: &str,
    ) -> Result<Vec<(PendingDeliveryKind, String, JsonValue)>> {
        let owner = envelope.event.owner();
        let name = envelope.event.name();
        let content = envelope.event.content();

        let actions = match owner.task {
            Some(task) => {
                let all = match self.cached_active_task_actions_by_task_id.entry(task) {
                    Entry::Occupied(cached) => cached.into_mut(),
                    Entry::Vacant(slot) => slot.insert(
                        active_actions_for_task(ci, task)
                            .await
                            .context("load the task's actions")?,
                    ),
                };
                matching_actions(all.clone(), &name, &content)
            }
            None => Vec::new(),
        };

        let candidates = match self
            .event_owners_with_webhooks
            .iter()
            .find(|(cached, _)| *cached == owner)
        {
            Some((_, hooks)) => hooks,
            None => {
                let hooks = gradient_ci::webhooks::candidates(&ci.db.worker_db, &owner)
                    .await
                    .context("load the event's webhooks")?;
                self.event_owners_with_webhooks.push((owner, hooks));
                &self.event_owners_with_webhooks[self.event_owners_with_webhooks.len() - 1].1
            }
        };
        let hooks: Vec<MWebhook> = candidates
            .iter()
            .filter(|h| {
                gradient_ci::webhooks::routes_to(h, &owner, &name, envelope.event.personal())
            })
            .cloned()
            .collect();

        let commit_status = match owner.task {
            Some(task) if check_context_kind_for_event(&name).is_some() => {
                let reports = match self.task_id_to_reports_commit_status.entry(task) {
                    Entry::Occupied(cached) => *cached.get(),
                    Entry::Vacant(slot) => *slot.insert(
                        load_task_git_host_connection(ci, task)
                            .await
                            .context("load the task's git host connection")?
                            .is_some_and(|(task, allowed)| {
                                task_should_post_commit_statuses(&task, allowed.connection())
                            }),
                    ),
                };
                match reports {
                    true => commit_status_check_updated_by_event_name_and_content(&name, &content)?
                        .map(|check| (task, check)),
                    false => None,
                }
            }
            _ => None,
        };

        Ok(plan_deliveries(
            envelope,
            &actions,
            &hooks,
            commit_status.as_ref(),
            parent,
        ))
    }
}

async fn enrich_events_plan_enqueue_and_run_reactions(
    ctx: &EffectsCtx,
    rows: &[PendingDelivery],
) -> Result<()> {
    let events = rows
        .iter()
        .map(decode_event_from_delivery_payload)
        .collect::<Result<Vec<_>>>()?;
    let db = ctx.db();
    let enriched =
        crate::enrich::fill_event_fields_in_batch(&db.worker_db, &db.store_path_mapping, events)
            .await?;
    let envelopes: Vec<(&str, Envelope)> = rows
        .iter()
        .zip(enriched)
        .filter_map(|(row, event)| Some((row.key.as_str(), Envelope::now(event?))))
        .collect();

    let ci = ctx.ci();
    let mut subscribers = EventSubscriberCache::default();
    let mut plan = Vec::new();
    for (parent, envelope) in &envelopes {
        plan.extend(
            subscribers
                .compute_event_deliveries_using_subscriber_cache(&ci, envelope, parent)
                .await?,
        );
    }
    insert_planned_deliveries_into_pending_delivery(&ci, plan).await?;

    for (_, envelope) in &envelopes {
        if let Event::EvaluationReported(reported) = &envelope.event {
            react_on_terminal(ctx, reported).await;
        }
    }
    Ok(())
}

/// The whole expansion is one transaction. Either every owed delivery is queued or none is. A
/// retried expansion cannot repeat an external call this way.
async fn insert_planned_deliveries_into_pending_delivery(
    ci: &CiContext,
    plan: Vec<(PendingDeliveryKind, String, JsonValue)>,
) -> Result<()> {
    if plan.is_empty() {
        return Ok(());
    }
    let mut by_kind: Vec<(PendingDeliveryKind, Vec<(String, JsonValue)>)> = Vec::new();
    for (kind, key, payload) in plan {
        match by_kind.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, rows)) => rows.push((key, payload)),
            None => by_kind.push((kind, vec![(key, payload)])),
        }
    }

    let txn = ci
        .db
        .worker_db
        .begin()
        .await
        .context("begin the delivery expansion")?;
    if let Err(e) = insert_planned_delivery_rows_by_kind(&txn, by_kind).await {
        txn.rollback()
            .await
            .context("roll back the delivery expansion")?;
        return Err(e);
    }
    txn.commit()
        .await
        .context("commit the delivery expansion")?;

    Ok(())
}

async fn insert_planned_delivery_rows_by_kind(
    txn: &DatabaseTransaction,
    by_kind: Vec<(PendingDeliveryKind, Vec<(String, JsonValue)>)>,
) -> Result<()> {
    for (kind, rows) in by_kind {
        match kind {
            PendingDeliveryKind::CommitStatusCheckDelivery => {
                enqueue_or_increment_revision_many(txn, kind, rows).await
            }
            _ => enqueue_many(txn, kind, rows).await,
        }
        .context("enqueue a delivery")?;
    }
    Ok(())
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
        CommitStatusCheckWithIds::Approval { approval, .. } => {
            format!("{task}:approval:{approval}")
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
    if reported.created {
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

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn an_eval_cache_compaction_queued_without_a_project_is_dropped() {
        let queued_before_the_upgrade = row(serde_json::json!({
            "cache": uuid::Uuid::now_v7(),
            "fingerprint": FINGERPRINT,
            "copies": [],
        }));

        assert!(
            decode_queued_eval_cache_compaction_from_delivery_payload(&queued_before_the_upgrade)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_eval_cache_compaction_runs_for_the_project_it_names() {
        let project = gradient_types::ids::ProjectId::now_v7();
        let queued = row(serde_json::json!({
            "cache": uuid::Uuid::now_v7(),
            "project": project,
            "fingerprint": FINGERPRINT,
            "copies": [],
        }));

        let compaction = decode_queued_eval_cache_compaction_from_delivery_payload(&queued)
            .unwrap()
            .expect("a compaction with a project runs");
        assert_eq!(compaction.store.project, project);
        assert_eq!(compaction.store.fingerprint.as_str(), FINGERPRINT);
    }

    fn event_row(event: impl Into<Event>) -> PendingDelivery {
        PendingDelivery {
            kind: PendingDeliveryKind::Event,
            key: uuid::Uuid::now_v7().to_string(),
            ..row(serde_json::to_value(event.into()).unwrap())
        }
    }

    fn audited(metadata: JsonValue) -> gradient_types::events::audit::Audited {
        gradient_types::events::audit::Audited {
            action: gradient_types::events::audit::Action::ProjectCreate,
            user: None,
            owner: EventOwner::default(),
            metadata: Some(metadata),
        }
    }

    #[test]
    fn split_claimed_rows_into_delivery_jobs_puts_database_only_event_rows_in_one_job_and_each_other_row_in_its_own_job()
     {
        let (first, second) = (
            event_row(audited(serde_json::json!({}))),
            event_row(audited(serde_json::json!({}))),
        );
        let evaluation_report = event_row(evaluation::Reported::default());
        let undecodable = PendingDelivery {
            kind: PendingDeliveryKind::Event,
            ..row(serde_json::json!({"NoSuchEvent": {}}))
        };
        let action = row(serde_json::json!({}));

        assert_eq!(
            split_claimed_rows_into_delivery_jobs(vec![
                first.clone(),
                evaluation_report.clone(),
                undecodable.clone(),
                second.clone(),
                action.clone(),
            ]),
            vec![
                ConsumedDeliveryWork::SinglePendingDelivery(evaluation_report),
                ConsumedDeliveryWork::SinglePendingDelivery(undecodable),
                ConsumedDeliveryWork::SinglePendingDelivery(action),
                ConsumedDeliveryWork::DatabaseOnlyEventExpansionBatch(vec![first, second]),
            ]
        );
    }

    #[tokio::test]
    async fn event_with_nul_payload_retried_alone_while_healthy_events_get_one_delivery() {
        use gradient_entity::pending_delivery;
        use gradient_entity::webhook::WebhookScope;
        use sea_orm::{ActiveModelTrait, IntoActiveModel};

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let user = gradient_fixtures::rows::user()
            .into_active_model()
            .insert(&db.connection())
            .await
            .expect("the user is stored");
        MWebhook {
            id: WebhookId::now_v7(),
            scope: WebhookScope::Instance,
            name: "all".into(),
            url: "https://example.com/hook".into(),
            events: serde_json::json!([]),
            active: true,
            created_by: user.id,
            ..Default::default()
        }
        .into_active_model()
        .insert(&db.connection())
        .await
        .expect("the webhook is stored");
        let state = gradient_test_support::state::TestState::new()
            .worker_db(db.connection())
            .build();
        let ctx = EffectsCtx::new(
            state.ci(),
            std::sync::Arc::new(gradient_scheduler::Scheduler::new(std::sync::Arc::clone(
                &state,
            ))),
        );
        let healthy = [
            event_row(audited(serde_json::json!({"note": "first"}))),
            event_row(audited(serde_json::json!({"note": "second"}))),
        ];
        let poisoned = event_row(audited(serde_json::json!({"note": "\u{0}"})));

        let outcomes = consume_delivery_job_with_row_outcomes(
            &ctx,
            ConsumedDeliveryWork::DatabaseOnlyEventExpansionBatch(vec![
                healthy[0].clone(),
                poisoned.clone(),
                healthy[1].clone(),
            ]),
        )
        .await;

        let outcome_of = |wanted: &PendingDelivery| {
            outcomes
                .iter()
                .find(|(row, _)| row.id == wanted.id)
                .map(|(_, outcome)| outcome.clone())
                .expect("every row has an outcome")
        };
        assert_eq!(outcomes.len(), 3);
        assert_eq!(outcome_of(&healthy[0]), Outcome::Delivered);
        assert_eq!(outcome_of(&healthy[1]), Outcome::Delivered);
        assert!(matches!(outcome_of(&poisoned), Outcome::Retry(_)));
        let mut webhook_delivery_parents: Vec<String> = db
            .rows::<pending_delivery::Entity>()
            .await
            .into_iter()
            .filter(|row| row.kind == PendingDeliveryKind::WebhookDelivery)
            .map(|row| row.key.rsplit(':').next().unwrap_or_default().to_owned())
            .collect();
        webhook_delivery_parents.sort();
        let mut healthy_keys: Vec<String> = healthy.iter().map(|row| row.key.clone()).collect();
        healthy_keys.sort();
        assert_eq!(webhook_delivery_parents, healthy_keys);

        drop((ctx, state));
        db.finish().await;
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
    fn evaluation_queued_maps_to_check_but_input_update_maps_to_none() {
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
