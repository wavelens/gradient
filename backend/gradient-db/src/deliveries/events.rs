/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::deliveries::pending::{PendingDeliveryKind, enqueue};
use gradient_types::events::{Event, EventBus, evaluation};
use gradient_types::{MEvaluation, WaitingReason};
use sea_orm::{ConnectionTrait, DbErr};

pub async fn record<C: ConnectionTrait>(
    db: &C,
    bus: &EventBus,
    event: impl Into<Event>,
) -> Result<(), DbErr> {
    let event = event.into();
    debug_assert!(event.durable(), "{} is firehose only", event.name());
    let key = event
        .key()
        .unwrap_or_else(|| format!("{}:{}", event.name(), uuid::Uuid::now_v7()));
    let payload = serde_json::to_value(&event).map_err(|e| DbErr::Custom(e.to_string()))?;
    enqueue(db, PendingDeliveryKind::Event, key, payload).await?;
    bus.publish(event);
    Ok(())
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
        status: i32::from(eval.status) as i16,
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
    use gradient_types::events::{EventBus, build, gc};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn one_row() -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }
    }

    #[tokio::test]
    async fn record_enqueues_one_event_row_and_publishes() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([one_row()])
            .into_connection();
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();

        record(
            &db,
            &bus,
            build::Reported {
                status: i32::from(BuildStatus::Completed) as i16,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let log = db.into_transaction_log();
        assert_eq!(log.len(), 1);
        let values = log[0].statements()[0].values.clone().unwrap().0;
        assert_eq!(values[1], sea_orm::Value::SmallInt(Some(4)));
        assert_eq!(rx.try_recv().unwrap().event.name(), "build.completed");
    }

    #[tokio::test]
    async fn an_event_without_a_key_gets_a_unique_one() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
