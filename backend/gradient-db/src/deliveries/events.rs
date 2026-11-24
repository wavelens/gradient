/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use crate::deliveries::pending::{PendingDeliveryKind, enqueue_many};
use gradient_entity::webhook::WebhookScope;
use gradient_types::events::{Event, EventBus, evaluation};
use gradient_types::ids::EvaluationId;
use gradient_types::{MEvaluation, WaitingReason};
use sea_orm::{ConnectionTrait, DbErr, Value};

crate::sql! {
    EVALUATION_IDS_WITH_REPORT_EVENT_CONSUMERS_QUERY = "SELECT e.id FROM evaluation e JOIN task t ON t.id = e.task \
         WHERE e.id = ANY($1::uuid[]) AND ( \
             EXISTS (SELECT 1 FROM task_action a WHERE a.task = t.id AND a.active) \
             OR (t.report_status AND EXISTS (SELECT 1 FROM git_host_connection c \
                 WHERE c.id = t.git_host_connection AND c.reports_status)) \
             OR EXISTS (SELECT 1 FROM webhook w WHERE w.active AND (w.scope = $2::smallint OR w.project = t.project)))",
        params = [EvaluationIds(64), Int(2)];
}

pub async fn record<C: ConnectionTrait>(
    db: &C,
    bus: &EventBus,
    event: impl Into<Event>,
) -> Result<(), DbErr> {
    record_many(db, bus, vec![event.into()]).await
}

pub async fn record_many<C: ConnectionTrait>(
    db: &C,
    bus: &EventBus,
    events: Vec<Event>,
) -> Result<(), DbErr> {
    let consumed = evaluation_ids_with_report_event_consumers(
        db,
        events
            .iter()
            .filter_map(evaluation_id_of_event_with_only_task_and_project_consumers)
            .collect(),
    )
    .await?;
    let rows = events
        .iter()
        .filter(|event| {
            evaluation_id_of_event_with_only_task_and_project_consumers(event)
                .is_none_or(|evaluation| consumed.contains(&evaluation))
        })
        .map(delivery_row)
        .collect::<Result<Vec<_>, DbErr>>()?;
    enqueue_many(db, PendingDeliveryKind::Event, rows).await?;
    for event in events {
        bus.publish(event);
    }

    Ok(())
}

/// Only evaluation tasks and projects have consumers for these events (task actions, status reports to the Git host, webhooks). The server doesn't store events without any of these consumers.
fn evaluation_id_of_event_with_only_task_and_project_consumers(
    event: &Event,
) -> Option<EvaluationId> {
    match event {
        Event::BuildReported(reported) => Some(reported.evaluation_id),
        Event::ExecJobReported(reported) => Some(reported.evaluation),
        _ => None,
    }
}

async fn evaluation_ids_with_report_event_consumers<C: ConnectionTrait>(
    db: &C,
    evaluations: HashSet<EvaluationId>,
) -> Result<HashSet<EvaluationId>, DbErr> {
    if evaluations.is_empty() {
        return Ok(HashSet::new());
    }

    let ids: Vec<uuid::Uuid> = evaluations
        .into_iter()
        .map(EvaluationId::into_inner)
        .collect();
    db.query_all_raw(EVALUATION_IDS_WITH_REPORT_EVENT_CONSUMERS_QUERY.bind([
        ids.into(),
        Value::SmallInt(Some(WebhookScope::Instance as i16)),
    ]))
    .await?
    .iter()
    .map(|row| Ok(EvaluationId::new(row.try_get::<uuid::Uuid>("", "id")?)))
    .collect()
}

fn delivery_row(event: &Event) -> Result<(String, serde_json::Value), DbErr> {
    debug_assert!(event.durable(), "{} is firehose only", event.name());
    let key = event
        .key()
        .unwrap_or_else(|| format!("{}:{}", event.name(), uuid::Uuid::now_v7()));
    let payload = serde_json::to_value(event).map_err(|e| DbErr::Custom(e.to_string()))?;
    Ok((key, payload))
}

/// A freshly inserted evaluation is never passing through `update_evaluation_status`.
/// Its Git host check is owed here instead.
pub fn evaluation_created(eval: &MEvaluation) -> Option<evaluation::Reported> {
    let task = eval.task?;
    let reason = eval
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json);
    let (phase, description) = evaluation::Phase::of_created(eval.status, reason)?;
    Some(evaluation::Reported {
        evaluation_id: eval.id,
        phase,
        status: i16::from(eval.status),
        task: Some(task),
        repository: Some(eval.repository.clone()),
        description: description.map(str::to_owned),
        created: true,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build::BuildStatus;
    use gradient_fixtures::mock::mock_db;
    use gradient_types::events::{EventBus, build, gc};
    use sea_orm::MockExecResult;
    use std::collections::BTreeMap;

    fn one_row() -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }
    }

    fn completed_build_of(evaluation_id: EvaluationId) -> Event {
        build::Reported {
            build_id: gradient_types::ids::BuildJobId::now_v7(),
            evaluation_id,
            status: i16::from(BuildStatus::Completed),
            ..Default::default()
        }
        .into()
    }

    #[tokio::test]
    async fn record_enqueues_one_event_row_and_publishes() {
        let evaluation = EvaluationId::now_v7();
        let db = mock_db()
            .append_query_results([vec![BTreeMap::from([(
                "id".to_owned(),
                Value::from(evaluation.into_inner()),
            )])]])
            .append_exec_results([one_row()])
            .into_connection();
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();

        record(&db, &bus, completed_build_of(evaluation))
            .await
            .unwrap();

        let log = db.into_transaction_log();
        assert_eq!(log.len(), 2);
        let values = log[1].statements()[0].values.clone().unwrap().0;
        assert_eq!(values[1], Value::SmallInt(Some(4)));
        assert_eq!(rx.try_recv().unwrap().event.name(), "build.completed");
    }

    #[tokio::test]
    async fn only_reports_of_evaluations_with_a_consumer_are_stored() {
        let (quiet, watched) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(quiet)
            .evaluation(watched)
            .flake_update_pull_request(&[])
            .into_database()
            .await;
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();

        record_many(
            &db.connection(),
            &bus,
            vec![completed_build_of(quiet), completed_build_of(watched)],
        )
        .await
        .expect("the reports are recorded");

        let rows = db.rows::<gradient_entity::pending_delivery::Entity>().await;
        assert!(
            rows.len() == 1 && rows[0].payload.to_string().contains(&watched.to_string()),
            "The code can store only the build of an evaluation with an active task action."
        );
        assert!(
            rx.try_recv().is_ok() && rx.try_recv().is_ok(),
            "Both events still reach the live event stream."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn an_event_without_a_key_gets_a_unique_one() {
        let db = mock_db()
            .append_exec_results([one_row(), one_row()])
            .into_connection();
        let bus = EventBus::new(4);
        let swept = gc::Swept {
            pass: gc::Pass::OrphanNars,
            removed: 1,
        };

        record(&db, &bus, swept.clone()).await.unwrap();
        record(&db, &bus, swept).await.unwrap();

        let log = db.into_transaction_log();
        let key = |i: usize| log[i].statements()[0].values.clone().unwrap().0[2].clone();
        assert_ne!(key(0), key(1));
    }

    #[test]
    fn a_taskless_evaluation_owes_no_first_report() {
        assert!(evaluation_created(&MEvaluation::default()).is_none());
    }
}
