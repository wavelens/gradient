/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The effects outbox: what a state change owes the outside world, written in
//! the transaction that made the change and delivered by the effects actor.
//!
//! Nothing here talks to the network and nothing here knows what a delivery is:
//! a row is a kind, a key that folds duplicates, and a payload. The claim leases
//! rows under `SKIP LOCKED` so two server instances never deliver the same row
//! at once, and a failure moves the row's next attempt out by [`backoff`] until
//! [`MAX_ATTEMPTS`], where it dead-letters in place for the retention pass.

use std::time::Duration;

use gradient_entity::outbox::OutboxKind;
use gradient_types::MEvaluation;
use gradient_types::ids::OutboxId;
use sea_orm::{ConnectionTrait, DbErr, Value};

/// Deliveries past this many failures are dead letters, not work.
pub const MAX_ATTEMPTS: i32 = 6;
/// How long a claimed row stays invisible to another claimer. Long enough that
/// a worker killed mid-delivery does not hand the row to a second one while the
/// first is still talking to a forge.
pub const CLAIM_LEASE_SECS: i64 = 600;

crate::sql! {
    /// A duplicate of a row still waiting is the row already waiting: the
    /// partial unique index folds it away rather than queueing a second report
    /// of the same status.
    OUTBOX_ENQUEUE = "INSERT INTO outbox (id, kind, key, payload, created_at, next_attempt_at) \
         VALUES ($1, $2::smallint, $3, $4::jsonb, (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC')) \
         ON CONFLICT (kind, key) WHERE delivered_at IS NULL AND failed_at IS NULL DO NOTHING",
        params = [NewUuid, Int(3), Text("outbox-gate-probe"), Text("{}")];

    /// The lease and the selection are one statement, so a row is never read by
    /// one claimer and leased by another.
    OUTBOX_CLAIM_DUE = "UPDATE outbox o SET next_attempt_at = (now() AT TIME ZONE 'UTC') + make_interval(secs => $2::int) \
         WHERE o.id IN ( \
             SELECT id FROM outbox \
             WHERE delivered_at IS NULL AND failed_at IS NULL \
               AND next_attempt_at <= (now() AT TIME ZONE 'UTC') \
             ORDER BY next_attempt_at, id LIMIT $1 FOR UPDATE SKIP LOCKED) \
         RETURNING o.id, o.kind, o.key, o.payload, o.attempts",
        params = [Int(8), Int(CLAIM_LEASE_SECS)];

    OUTBOX_MARK_DELIVERED = "UPDATE outbox SET delivered_at = (now() AT TIME ZONE 'UTC') WHERE id = $1",
        params = [NewUuid];

    /// The attempt count, the dead-letter decision and the next attempt move
    /// together: reading `attempts` first and writing it back would lose a
    /// concurrent retry of the same row.
    OUTBOX_MARK_RETRY = "UPDATE outbox SET attempts = attempts + 1, last_error = $2, \
         failed_at = CASE WHEN attempts + 1 >= $3 THEN (now() AT TIME ZONE 'UTC') ELSE NULL END, \
         next_attempt_at = (now() AT TIME ZONE 'UTC') + make_interval(secs => $4::int) \
         WHERE id = $1",
        params = [NewUuid, Text("probe"), Int(MAX_ATTEMPTS as i64), Int(30)];

    OUTBOX_PENDING_COUNTS = "SELECT count(*) FILTER (WHERE delivered_at IS NULL AND failed_at IS NULL) AS pending, \
         count(*) FILTER (WHERE failed_at IS NOT NULL) AS failed FROM outbox",
        params = [],
        tier = Sweep;
}

/// Doubling from 30 s, capped at 15 minutes: a forge that 502s once is retried
/// while the check still matters, and one that is down stops costing attempts.
pub fn backoff(attempts: i32) -> Duration {
    let shift = u32::try_from(attempts.clamp(0, 20)).unwrap_or(0);
    Duration::from_secs(30u64.saturating_mul(1u64 << shift).min(900))
}

/// A claimed row: what the deliverer needs and nothing the delivery cannot use.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    pub id: OutboxId,
    pub kind: OutboxKind,
    pub key: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    Retry(String),
}

pub async fn enqueue<C: ConnectionTrait>(
    db: &C,
    kind: OutboxKind,
    key: String,
    payload: serde_json::Value,
) -> Result<(), DbErr> {
    db.execute_raw(OUTBOX_ENQUEUE.bind([
        Value::Uuid(Some(OutboxId::now_v7().into_inner())),
        Value::SmallInt(Some(i16::from(kind))),
        Value::String(Some(key)),
        Value::String(Some(payload.to_string())),
    ]))
    .await?;

    Ok(())
}

/// The first forge report for a freshly inserted evaluation, which never
/// transitions through `update_evaluation_status` and so is never announced by
/// the transition emitter.
pub async fn enqueue_evaluation_created<C: ConnectionTrait>(
    db: &C,
    eval: &MEvaluation,
) -> Result<(), DbErr> {
    let Some(task) = eval.task else {
        return Ok(());
    };

    enqueue(
        db,
        OutboxKind::EvaluationStatus,
        format!("{}:{}", eval.id, i32::from(eval.status)),
        serde_json::json!({
            "evaluation": eval.id,
            "task": task,
            "status": i32::from(eval.status),
            "waiting_reason": eval.waiting_reason,
            "repository": eval.repository,
            "created": true,
        }),
    )
    .await
}

pub async fn claim_due<C: ConnectionTrait>(db: &C, limit: usize) -> Result<Vec<OutboxRow>, DbErr> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(OUTBOX_CLAIM_DUE.bind([
            Value::BigInt(Some(limit as i64)),
            Value::BigInt(Some(CLAIM_LEASE_SECS)),
        ]))
        .await?;

    rows.iter()
        .map(|r| {
            let kind = r.try_get::<i16>("", "kind")?;
            Ok(OutboxRow {
                id: OutboxId::new(r.try_get::<uuid::Uuid>("", "id")?),
                kind: OutboxKind::try_from(kind)
                    .map_err(|_| DbErr::Custom(format!("unknown outbox kind {kind}")))?,
                key: r.try_get("", "key")?,
                payload: r.try_get("", "payload")?,
                attempts: r.try_get("", "attempts")?,
            })
        })
        .collect()
}

pub async fn mark<C: ConnectionTrait>(
    db: &C,
    row: &OutboxRow,
    outcome: &Outcome,
) -> Result<(), DbErr> {
    let stmt = match outcome {
        Outcome::Delivered => OUTBOX_MARK_DELIVERED.bind([Value::Uuid(Some(row.id.into_inner()))]),
        Outcome::Retry(error) => OUTBOX_MARK_RETRY.bind([
            Value::Uuid(Some(row.id.into_inner())),
            Value::String(Some(error.chars().take(4000).collect())),
            Value::Int(Some(MAX_ATTEMPTS)),
            Value::BigInt(Some(backoff(row.attempts).as_secs() as i64)),
        ]),
    };
    db.execute_raw(stmt).await?;

    Ok(())
}

/// `(pending, dead-lettered)`, for `/board/health`.
pub async fn pending_counts<C: ConnectionTrait>(db: &C) -> Result<(i64, i64), DbErr> {
    let row = db.query_one_raw(OUTBOX_PENDING_COUNTS.stmt()).await?;

    Ok(row
        .map(|r| {
            (
                r.try_get("", "pending").unwrap_or(0),
                r.try_get("", "failed").unwrap_or(0),
            )
        })
        .unwrap_or((0, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::BTreeMap;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn backoff_doubles_from_thirty_seconds_and_caps_at_fifteen_minutes() {
        assert_eq!(backoff(0), Duration::from_secs(30));
        assert_eq!(backoff(1), Duration::from_secs(60));
        assert_eq!(backoff(4), Duration::from_secs(480));
        assert_eq!(backoff(5), Duration::from_secs(900));
        assert_eq!(backoff(20), Duration::from_secs(900));
    }

    /// Claiming is one statement: the due rows are leased in the same UPDATE
    /// that selects them, under SKIP LOCKED, oldest first.
    #[tokio::test]
    async fn claim_due_leases_in_one_skip_locked_statement() {
        let id = OutboxId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([
                ("id".to_owned(), Value::from(id.into_inner())),
                ("kind".to_owned(), Value::SmallInt(Some(3))),
                ("key".to_owned(), Value::from("k")),
                (
                    "payload".to_owned(),
                    Value::Json(Some(Box::new(serde_json::json!({"a": 1})))),
                ),
                ("attempts".to_owned(), Value::Int(Some(0))),
            ])]])
            .into_connection();

        let rows = claim_due(&db, 8).await.unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, OutboxKind::ActionDelivery);
        let sql = norm(&db.into_transaction_log()[0].statements()[0].sql);
        assert!(
            sql.starts_with(
                "UPDATE outbox o SET next_attempt_at = (now() AT TIME ZONE 'UTC') + make_interval(secs => $2::int)"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("ORDER BY next_attempt_at, id LIMIT $1 FOR UPDATE SKIP LOCKED"),
            "{sql}"
        );
        assert!(
            sql.ends_with("RETURNING o.id, o.kind, o.key, o.payload, o.attempts"),
            "{sql}"
        );
    }

    /// Nothing is claimed for no capacity, and no statement is sent to find
    /// that out.
    #[tokio::test]
    async fn claim_due_asks_for_nothing_when_there_is_no_room() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        assert!(claim_due(&db, 0).await.unwrap().is_empty());
        assert!(db.into_transaction_log().is_empty());
    }

    /// A failed delivery moves its next attempt out by the backoff and
    /// dead-letters at the cap, in one statement.
    #[tokio::test]
    async fn mark_retry_schedules_and_dead_letters_at_the_cap() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let row = OutboxRow {
            id: OutboxId::now_v7(),
            kind: OutboxKind::ActionDelivery,
            key: "k".into(),
            payload: serde_json::json!({}),
            attempts: 0,
        };
        mark(&db, &row, &Outcome::Retry("502".into()))
            .await
            .unwrap();

        let log = db.into_transaction_log();
        let sql = norm(&log[0].statements()[0].sql);
        assert!(sql.contains("attempts = attempts + 1"), "{sql}");
        assert!(
            sql.contains(
                "failed_at = CASE WHEN attempts + 1 >= $3 THEN (now() AT TIME ZONE 'UTC') ELSE NULL END"
            ),
            "{sql}"
        );
        assert!(
            sql.contains(
                "next_attempt_at = (now() AT TIME ZONE 'UTC') + make_interval(secs => $4::int)"
            ),
            "{sql}"
        );
        let values = log[0].statements()[0].values.clone().unwrap().0;
        assert_eq!(
            values[3],
            Value::BigInt(Some(30)),
            "the first retry waits one backoff step"
        );
    }

    /// A duplicate pending event is folded into the one already waiting.
    #[tokio::test]
    async fn enqueue_does_nothing_on_a_pending_duplicate_key() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        enqueue(
            &db,
            OutboxKind::BuildStatus,
            "j:3".into(),
            serde_json::json!({}),
        )
        .await
        .unwrap();

        let sql = norm(&db.into_transaction_log()[0].statements()[0].sql);
        assert!(
            sql.contains(
                "ON CONFLICT (kind, key) WHERE delivered_at IS NULL AND failed_at IS NULL DO NOTHING"
            ),
            "{sql}"
        );
    }

    /// An evaluation with no task has no forge to report to, so it enqueues
    /// nothing rather than a row every consumer would drop.
    #[tokio::test]
    async fn a_taskless_evaluation_enqueues_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        enqueue_evaluation_created(&db, &MEvaluation::default())
            .await
            .unwrap();

        assert!(db.into_transaction_log().is_empty());
    }
}
