/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

pub use gradient_entity::pending_delivery::PendingDeliveryKind;
use gradient_types::ids::PendingDeliveryId;
use sea_orm::{ConnectionTrait, DbErr, Value};

pub const MAX_ATTEMPTS: i32 = 6;
pub const CLAIM_LEASE_SECS: i64 = 600;

crate::sql! {
    PENDING_DELIVERY_ENQUEUE = "INSERT INTO pending_delivery (id, kind, key, payload, created_at, next_attempt_at) \
         VALUES ($1, $2::smallint, $3, $4::jsonb, (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC')) \
         ON CONFLICT (kind, key) WHERE delivered_at IS NULL AND failed_at IS NULL DO NOTHING",
        params = [NewUuid, Int(3), Text("pending-delivery-gate-probe"), Text("{}")];

    PENDING_DELIVERY_ENQUEUE_AFTER = "INSERT INTO pending_delivery (id, kind, key, payload, created_at, next_attempt_at) \
         VALUES ($1, $2::smallint, $3, $4::jsonb, (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC') + make_interval(secs => $5::int)) \
         ON CONFLICT (kind, key) WHERE delivered_at IS NULL AND failed_at IS NULL DO NOTHING",
        params = [NewUuid, Int(7), Text("pending-delivery-gate-probe"), Text("{}"), Int(900)];

    PENDING_DELIVERY_UPSERT_UNSETTLED = "INSERT INTO pending_delivery (id, kind, key, payload, created_at, next_attempt_at) \
         VALUES ($1, $2::smallint, $3, $4::jsonb, (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC')) \
         ON CONFLICT (kind, key) WHERE delivered_at IS NULL AND failed_at IS NULL \
         DO UPDATE SET revision = pending_delivery.revision + 1, payload = EXCLUDED.payload, attempts = 0",
        params = [NewUuid, Int(8), Text("pending-delivery-gate-probe"), Text("{}")];

    PENDING_DELIVERY_ENQUEUE_MANY = "INSERT INTO pending_delivery (id, kind, key, payload, created_at, next_attempt_at) \
         SELECT r.id, $2::smallint, r.key, r.payload::jsonb, (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC') \
         FROM unnest($1::uuid[], $3::text[], $4::text[]) AS r(id, key, payload) \
         ON CONFLICT (kind, key) WHERE delivered_at IS NULL AND failed_at IS NULL DO NOTHING",
        params = [NewUuids(64), Int(3), Texts("pending-delivery-gate-probe", 64), Texts("{}", 64)];

    PENDING_DELIVERY_CLAIM_DUE = "UPDATE pending_delivery o SET next_attempt_at = (now() AT TIME ZONE 'UTC') + make_interval(secs => $2::int) \
         WHERE o.id IN ( \
             SELECT id FROM pending_delivery \
             WHERE delivered_at IS NULL AND failed_at IS NULL \
               AND next_attempt_at <= (now() AT TIME ZONE 'UTC') \
               AND kind IN (2, 3, 4, 5, 6, 7, 8, 9) \
             ORDER BY next_attempt_at, id LIMIT $1 FOR UPDATE SKIP LOCKED) \
         RETURNING o.id, o.kind, o.key, o.payload, o.attempts, o.revision",
        params = [Int(8), Int(CLAIM_LEASE_SECS)];

    PENDING_DELIVERY_UPSERT_MANY_UNSETTLED = "INSERT INTO pending_delivery (id, kind, key, payload, created_at, next_attempt_at) \
         SELECT r.id, $2::smallint, r.key, r.payload::jsonb, (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC') \
         FROM unnest($1::uuid[], $3::text[], $4::text[]) AS r(id, key, payload) \
         ON CONFLICT (kind, key) WHERE delivered_at IS NULL AND failed_at IS NULL \
         DO UPDATE SET revision = pending_delivery.revision + 1, payload = EXCLUDED.payload, attempts = 0",
        params = [NewUuids(1), Int(8), Texts("pending-delivery-gate-probe", 1), Texts("{}", 1)];

    /// Attempt count, dead-letter check and next attempt happen in one statement. Any separate write after the read of `attempts` can drop concurrent retries on the same row.
    RECORD_CLAIMED_PENDING_DELIVERY_OUTCOMES = "UPDATE pending_delivery p SET \
             attempts = CASE WHEN r.outcome = 'delivered' THEN p.attempts ELSE p.attempts + 1 END, \
             last_error = CASE WHEN r.outcome = 'delivered' THEN p.last_error ELSE r.error END, \
             delivered_at = CASE WHEN r.outcome = 'delivered' AND p.revision = r.revision THEN (now() AT TIME ZONE 'UTC') END, \
             failed_at = CASE WHEN r.outcome <> 'delivered' \
                 AND (p.attempts + 1 >= $6 OR (r.outcome = 'failed' AND p.revision = r.revision)) \
                 THEN (now() AT TIME ZONE 'UTC') END, \
             next_attempt_at = CASE \
                 WHEN p.revision <> r.revision OR r.outcome = 'failed' THEN (now() AT TIME ZONE 'UTC') \
                 WHEN r.outcome = 'retry' THEN (now() AT TIME ZONE 'UTC') + make_interval(secs => r.retry_after_secs) \
                 ELSE p.next_attempt_at END \
         FROM unnest($1::uuid[], $2::int[], $3::text[], $4::text[], $5::int[]) AS r(id, revision, outcome, error, retry_after_secs) \
         WHERE p.id = r.id AND p.delivered_at IS NULL AND p.failed_at IS NULL",
        params = [NewUuids(64), Ints(0, 64), Texts("retry", 64), Texts("probe", 64), Ints(30, 64), Int(MAX_ATTEMPTS as i64)];

    PENDING_DELIVERY_PENDING_COUNTS = "SELECT count(*) FILTER (WHERE delivered_at IS NULL AND failed_at IS NULL) AS pending, \
         count(*) FILTER (WHERE failed_at IS NOT NULL) AS failed FROM pending_delivery",
        params = [],
        tier = Sweep;
}

pub fn backoff(attempts: i32) -> Duration {
    let shift = u32::try_from(attempts.clamp(0, 20)).unwrap_or(0);
    Duration::from_secs(30u64.saturating_mul(1u64 << shift).min(900))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingDelivery {
    pub id: PendingDeliveryId,
    pub kind: PendingDeliveryKind,
    pub key: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub revision: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    Retry(String),
    RetryAfterDelay(String, Duration),
    PermanentFailure(String),
}

pub async fn enqueue<C: ConnectionTrait>(
    db: &C,
    kind: PendingDeliveryKind,
    key: String,
    payload: serde_json::Value,
) -> Result<(), DbErr> {
    db.execute_raw(PENDING_DELIVERY_ENQUEUE.bind([
        Value::Uuid(Some(PendingDeliveryId::now_v7().into_inner())),
        Value::SmallInt(Some(i16::from(kind))),
        Value::String(Some(key)),
        Value::String(Some(payload.to_string())),
    ]))
    .await?;

    Ok(())
}

pub async fn enqueue_or_increment_revision<C: ConnectionTrait>(
    db: &C,
    kind: PendingDeliveryKind,
    key: String,
    payload: serde_json::Value,
) -> Result<(), DbErr> {
    db.execute_raw(PENDING_DELIVERY_UPSERT_UNSETTLED.bind([
        Value::Uuid(Some(PendingDeliveryId::now_v7().into_inner())),
        Value::SmallInt(Some(i16::from(kind))),
        Value::String(Some(key)),
        Value::String(Some(payload.to_string())),
    ]))
    .await?;

    Ok(())
}

pub async fn enqueue_after<C: ConnectionTrait>(
    db: &C,
    kind: PendingDeliveryKind,
    key: String,
    payload: serde_json::Value,
    delay: Duration,
) -> Result<(), DbErr> {
    db.execute_raw(PENDING_DELIVERY_ENQUEUE_AFTER.bind([
        Value::Uuid(Some(PendingDeliveryId::now_v7().into_inner())),
        Value::SmallInt(Some(i16::from(kind))),
        Value::String(Some(key)),
        Value::String(Some(payload.to_string())),
        Value::BigInt(Some(delay.as_secs() as i64)),
    ]))
    .await?;

    Ok(())
}

pub async fn enqueue_many<C: ConnectionTrait>(
    db: &C,
    kind: PendingDeliveryKind,
    rows: Vec<(String, serde_json::Value)>,
) -> Result<(), DbErr> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut rows = rows;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup_by(|a, b| a.0 == b.0);

    db.execute_raw(
        PENDING_DELIVERY_ENQUEUE_MANY.bind(build_pending_delivery_insert_bind_values(kind, rows)),
    )
    .await?;

    Ok(())
}

/// Repeated keys use the last payload. Keys go in sorted order, so overlapping concurrent batches lock rows in the same order.
pub async fn enqueue_or_increment_revision_many<C: ConnectionTrait>(
    db: &C,
    kind: PendingDeliveryKind,
    rows: Vec<(String, serde_json::Value)>,
) -> Result<(), DbErr> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut rows = rows;
    rows.reverse();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup_by(|a, b| a.0 == b.0);

    db.execute_raw(
        PENDING_DELIVERY_UPSERT_MANY_UNSETTLED
            .bind(build_pending_delivery_insert_bind_values(kind, rows)),
    )
    .await?;

    Ok(())
}

fn build_pending_delivery_insert_bind_values(
    kind: PendingDeliveryKind,
    rows: Vec<(String, serde_json::Value)>,
) -> [Value; 4] {
    let ids: Vec<uuid::Uuid> = rows
        .iter()
        .map(|_| PendingDeliveryId::now_v7().into_inner())
        .collect();
    let (keys, payloads): (Vec<String>, Vec<String>) = rows
        .into_iter()
        .map(|(key, payload)| (key, payload.to_string()))
        .unzip();
    [
        ids.into(),
        Value::SmallInt(Some(i16::from(kind))),
        keys.into(),
        payloads.into(),
    ]
}

pub async fn claim_due<C: ConnectionTrait>(
    db: &C,
    limit: usize,
) -> Result<Vec<PendingDelivery>, DbErr> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(PENDING_DELIVERY_CLAIM_DUE.bind([
            Value::BigInt(Some(limit as i64)),
            Value::BigInt(Some(CLAIM_LEASE_SECS)),
        ]))
        .await?;

    rows.iter()
        .map(|r| {
            let kind = r.try_get::<i16>("", "kind")?;
            Ok(PendingDelivery {
                id: PendingDeliveryId::new(r.try_get::<uuid::Uuid>("", "id")?),
                kind: PendingDeliveryKind::try_from(kind)
                    .map_err(|_| DbErr::Custom(format!("unknown pending delivery kind {kind}")))?,
                key: r.try_get("", "key")?,
                payload: r.try_get("", "payload")?,
                attempts: r.try_get("", "attempts")?,
                revision: r.try_get("", "revision")?,
            })
        })
        .collect()
}

pub async fn mark<C: ConnectionTrait>(
    db: &C,
    settled: &[(PendingDelivery, Outcome)],
) -> Result<(), DbErr> {
    if settled.is_empty() {
        return Ok(());
    }

    let mut ids = Vec::with_capacity(settled.len());
    let mut revisions = Vec::with_capacity(settled.len());
    let mut outcomes = Vec::with_capacity(settled.len());
    let mut errors = Vec::with_capacity(settled.len());
    let mut retry_after_secs = Vec::with_capacity(settled.len());
    for (row, outcome) in settled {
        let (name, error, delay) = match outcome {
            Outcome::Delivered => ("delivered", "", Duration::ZERO),
            Outcome::Retry(error) => ("retry", error.as_str(), backoff(row.attempts)),
            Outcome::RetryAfterDelay(error, delay) => {
                ("retry", error.as_str(), backoff(row.attempts).max(*delay))
            }
            Outcome::PermanentFailure(error) => ("failed", error.as_str(), Duration::ZERO),
        };
        ids.push(row.id.into_inner());
        revisions.push(row.revision);
        outcomes.push(name.to_owned());
        errors.push(truncate_delivery_error_message(error));
        retry_after_secs.push(i32::try_from(delay.as_secs()).unwrap_or(i32::MAX));
    }

    db.execute_raw(RECORD_CLAIMED_PENDING_DELIVERY_OUTCOMES.bind([
        ids.into(),
        revisions.into(),
        outcomes.into(),
        errors.into(),
        retry_after_secs.into(),
        Value::Int(Some(MAX_ATTEMPTS)),
    ]))
    .await?;

    Ok(())
}

fn truncate_delivery_error_message(error: &str) -> String {
    error.chars().take(4000).collect()
}

pub async fn pending_counts<C: ConnectionTrait>(db: &C) -> Result<(i64, i64), DbErr> {
    let row = db
        .query_one_raw(PENDING_DELIVERY_PENDING_COUNTS.stmt())
        .await?;

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
    use gradient_entity::pending_delivery;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use sea_orm::{DatabaseConnection, MockExecResult};

    #[test]
    fn backoff_doubles_from_thirty_seconds_and_caps_at_fifteen_minutes() {
        assert_eq!(backoff(0), Duration::from_secs(30));
        assert_eq!(backoff(1), Duration::from_secs(60));
        assert_eq!(backoff(4), Duration::from_secs(480));
        assert_eq!(backoff(5), Duration::from_secs(900));
        assert_eq!(backoff(20), Duration::from_secs(900));
    }

    #[tokio::test]
    async fn claim_due_returns_only_known_delivery_kinds_and_leases_rows() {
        use sea_orm::Iterable;

        let db = TestDatabase::create().await;
        let conn = db.connection();
        for kind in PendingDeliveryKind::iter() {
            enqueue(&conn, kind, "k".into(), serde_json::json!({}))
                .await
                .expect("the delivery enters the queue");
        }
        db.execute(
            "INSERT INTO pending_delivery (id, kind, key, payload, created_at, next_attempt_at) \
             VALUES (gen_random_uuid(), 32000, 'unknown', '{}', now(), now())",
        )
        .await;

        let claimed = claim_due(&conn, 64).await.unwrap();
        let claimed_again = claim_due(&conn, 64).await.unwrap();

        let mut kinds: Vec<i16> = claimed.iter().map(|row| i16::from(row.kind)).collect();
        kinds.sort_unstable();
        let mut known: Vec<i16> = PendingDeliveryKind::iter().map(i16::from).collect();
        known.sort_unstable();
        assert_eq!(kinds, known);
        assert!(claimed_again.is_empty(), "a claimed row is leased");
        assert_eq!(
            db.texts("SELECT count(*)::text FROM pending_delivery")
                .await,
            [(known.len() + 1).to_string()]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn claim_due_asks_for_nothing_when_there_is_no_room() {
        let db = empty_db();

        assert!(claim_due(&db, 0).await.unwrap().is_empty());
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn mark_records_a_whole_set_of_outcomes_in_one_statement() {
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 4,
            }])
            .into_connection();
        let row = || PendingDelivery {
            id: PendingDeliveryId::now_v7(),
            kind: PendingDeliveryKind::ActionDelivery,
            key: "k".into(),
            payload: serde_json::json!({}),
            attempts: 0,
            revision: 0,
        };

        mark(
            &db,
            &[
                (row(), Outcome::Delivered),
                (row(), Outcome::Retry("502".into())),
                (
                    row(),
                    Outcome::RetryAfterDelay("429".into(), Duration::from_secs(1200)),
                ),
                (row(), Outcome::PermanentFailure("422".into())),
            ],
        )
        .await
        .unwrap();

        let log = db.into_transaction_log();
        assert_eq!(log.len(), 1, "Three results, one SQL statement");
        let values = log[0].statements()[0].values.clone().unwrap().0;
        assert_eq!(
            values[2],
            Value::from(
                ["delivered", "retry", "retry", "failed"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(
            values[4],
            Value::from(vec![0i32, 30, 1200, 0]),
            "The first retry can wait one backoff step. A rate-limited row can wait for its own delay."
        );
    }

    #[tokio::test]
    async fn mark_of_a_mixed_set_settles_retries_and_dead_letters_each_row() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        for key in ["a", "b", "c"] {
            enqueue(
                &conn,
                PendingDeliveryKind::ActionDelivery,
                key.into(),
                serde_json::json!({}),
            )
            .await
            .expect("the delivery enters the queue");
        }
        let claimed = claim_due_rows_or_panic(&conn).await;
        let by_key = |key: &str| {
            claimed
                .iter()
                .find(|r| r.key == key)
                .expect("every row is claimed")
                .clone()
        };

        mark(
            &conn,
            &[
                (by_key("a"), Outcome::Delivered),
                (by_key("b"), Outcome::Retry("502".into())),
                (by_key("c"), Outcome::PermanentFailure("422".into())),
            ],
        )
        .await
        .expect("mark() recorded the set");

        mark(
            &conn,
            &[
                (by_key("a"), Outcome::Retry("late".into())),
                (by_key("c"), Outcome::Delivered),
            ],
        )
        .await
        .expect("late results are accepted");

        let mut rows = db.rows::<pending_delivery::Entity>().await;
        rows.sort_by(|x, y| x.key.cmp(&y.key));
        assert_eq!(
            rows.iter()
                .map(|r| (
                    r.key.as_str(),
                    r.delivered_at.is_some(),
                    r.failed_at.is_some(),
                    r.attempts,
                    r.last_error.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("a", true, false, 0, None),
                ("b", false, false, 1, Some("502")),
                ("c", false, true, 1, Some("422")),
            ],
            "Finalize delivered rows, schedule retries and dead-letter permanent failures in one statement."
        );
        assert!(
            claim_due_rows_or_panic(&conn).await.is_empty(),
            "Retried rows wait for backoff. The remaining two rows finished."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn enqueue_or_increment_revision_many_keeps_the_last_payload_of_a_repeated_key() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        enqueue_commit_status_post_with_event_name(&conn, "queued").await;

        enqueue_or_increment_revision_many(
            &conn,
            PendingDeliveryKind::CommitStatusCheckDelivery,
            vec![
                (
                    TEST_BUILD_PENDING_DELIVERY_KEY.into(),
                    serde_json::json!({ "event": "started" }),
                ),
                (
                    TEST_BUILD_PENDING_DELIVERY_KEY.into(),
                    serde_json::json!({ "event": "completed" }),
                ),
            ],
        )
        .await
        .expect("the deliveries enter the queue");

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert_eq!(
            rows.iter()
                .map(|r| (r.revision, r.payload.clone()))
                .collect::<Vec<_>>(),
            vec![(1, serde_json::json!({ "event": "completed" }))],
            "Duplicate keys keep the last payload and increment the revision once."
        );
        db.finish().await;
    }

    const TEST_BUILD_PENDING_DELIVERY_KEY: &str = "task:build:7";

    async fn enqueue_commit_status_post_with_event_name(db: &DatabaseConnection, event: &str) {
        enqueue_or_increment_revision(
            db,
            PendingDeliveryKind::CommitStatusCheckDelivery,
            TEST_BUILD_PENDING_DELIVERY_KEY.into(),
            serde_json::json!({ "event": event }),
        )
        .await
        .expect("the delivery enters the queue");
    }

    async fn claim_due_rows_or_panic(db: &DatabaseConnection) -> Vec<PendingDelivery> {
        claim_due(db, 8)
            .await
            .expect("claim_due leased a due delivery")
    }

    #[tokio::test]
    async fn enqueue_twice_with_same_key_keeps_one_row_with_revision_one_and_second_payload() {
        let db = TestDatabase::create().await;
        let conn = db.connection();

        enqueue_commit_status_post_with_event_name(&conn, "queued").await;
        enqueue_commit_status_post_with_event_name(&conn, "started").await;

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].revision, 1);
        assert_eq!(rows[0].payload, serde_json::json!({ "event": "started" }));
        db.finish().await;
    }

    #[tokio::test]
    async fn mark_delivered_keeps_row_unsettled_when_revision_changed_since_claim_and_makes_it_due_again()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        enqueue_commit_status_post_with_event_name(&conn, "queued").await;
        let claimed = claim_due_rows_or_panic(&conn).await;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].revision, 0);

        enqueue_commit_status_post_with_event_name(&conn, "started").await;
        mark(&conn, &[(claimed[0].clone(), Outcome::Delivered)])
            .await
            .expect("mark() recorded the delivery");

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert_eq!(
            rows[0].delivered_at, None,
            "change during claim must trigger a repost of the row"
        );
        let again = claim_due_rows_or_panic(&conn).await;
        assert_eq!(
            again.iter().map(|r| r.revision).collect::<Vec<_>>(),
            vec![1],
            "the released row is due at once with the newer revision"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn mark_failed_does_not_dead_letter_row_when_revision_changed_since_claim_and_makes_it_due_again()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        enqueue_commit_status_post_with_event_name(&conn, "queued").await;
        let claimed = claim_due_rows_or_panic(&conn).await;

        enqueue_commit_status_post_with_event_name(&conn, "started").await;
        mark(
            &conn,
            &[(claimed[0].clone(), Outcome::PermanentFailure("422".into()))],
        )
        .await
        .expect("mark() recorded a permanent failure");

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert_eq!(
            (
                rows[0].failed_at,
                rows[0].attempts,
                rows[0].last_error.as_deref()
            ),
            (None, 1, Some("422")),
            "Post the newer state for a change after claim. Don't abandon."
        );
        let again = claim_due_rows_or_panic(&conn).await;
        assert_eq!(
            again.iter().map(|r| r.revision).collect::<Vec<_>>(),
            vec![1],
            "the released row is due at once with the newer revision"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn mark_failed_dead_letters_row_when_revision_changed_since_claim_and_attempt_cap_reached()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        enqueue_commit_status_post_with_event_name(&conn, "queued").await;
        let claimed = claim_due_rows_or_panic(&conn).await;
        enqueue_commit_status_post_with_event_name(&conn, "started").await;
        for _ in 0..MAX_ATTEMPTS - 1 {
            mark(&conn, &[(claimed[0].clone(), Outcome::Retry("502".into()))])
                .await
                .expect("Retries before the limit leave that row unsettled.");
        }

        mark(
            &conn,
            &[(claimed[0].clone(), Outcome::PermanentFailure("401".into()))],
        )
        .await
        .expect("mark() recorded a permanent failure");

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert_eq!(
            (
                rows[0].revision,
                rows[0].attempts,
                rows[0].failed_at.is_some()
            ),
            (1, MAX_ATTEMPTS, true),
            "A changed row is still dead-letter at the attempt cap."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn failed_retry_with_new_revision_does_not_mark_row_failed_because_new_payload_has_zero_attempts()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        enqueue_commit_status_post_with_event_name(&conn, "queued").await;
        let claimed = claim_due_rows_or_panic(&conn).await;
        for _ in 0..MAX_ATTEMPTS - 1 {
            mark(&conn, &[(claimed[0].clone(), Outcome::Retry("502".into()))])
                .await
                .expect("Retries before the limit leave that row unsettled.");
        }

        enqueue_commit_status_post_with_event_name(&conn, "completed").await;
        let newer = PendingDelivery {
            revision: 1,
            attempts: 0,
            ..claimed[0].clone()
        };
        mark(&conn, &[(newer.clone(), Outcome::Retry("502".into()))])
            .await
            .expect("Expected `mark()` to record the retry");

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert_eq!(
            (rows[0].attempts, rows[0].failed_at.is_some()),
            (1, false),
            "A newer payload can start with a fresh retry budget."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn test_retry_is_due_immediately_after_revision_bump() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        enqueue_commit_status_post_with_event_name(&conn, "queued").await;
        let claimed = claim_due_rows_or_panic(&conn).await;

        enqueue_commit_status_post_with_event_name(&conn, "started").await;
        mark(&conn, &[(claimed[0].clone(), Outcome::Retry("502".into()))])
            .await
            .expect("Expected `mark()` to record the retry");

        let again = claim_due_rows_or_panic(&conn).await;
        assert_eq!(
            again.iter().map(|r| r.revision).collect::<Vec<_>>(),
            vec![1],
            "Retries of an old claim set the newer payload due immediately."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn claim_and_mark_delivered_with_unchanged_revision_sets_delivered_at() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        enqueue_commit_status_post_with_event_name(&conn, "queued").await;
        let claimed = claim_due_rows_or_panic(&conn).await;

        mark(&conn, &[(claimed[0].clone(), Outcome::Delivered)])
            .await
            .expect("mark() recorded the delivery");

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert!(
            rows[0].delivered_at.is_some(),
            "row settled with unchanged revision"
        );
        assert!(
            claim_due_rows_or_panic(&conn).await.is_empty(),
            "no second fetch for settled rows"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn plain_enqueue_keeps_unsettled_row_revision_and_payload_unchanged() {
        let db = TestDatabase::create().await;
        let conn = db.connection();

        for event in ["queued", "started"] {
            enqueue(
                &conn,
                PendingDeliveryKind::CommitStatusCheckDelivery,
                TEST_BUILD_PENDING_DELIVERY_KEY.into(),
                serde_json::json!({ "event": event }),
            )
            .await
            .expect("the delivery enters the queue");
        }

        let rows = db.rows::<pending_delivery::Entity>().await;
        assert_eq!(
            rows.iter()
                .map(|r| (r.revision, r.payload.clone()))
                .collect::<Vec<_>>(),
            vec![(0, serde_json::json!({ "event": "queued" }))],
            "plain enqueue operations keep the first payload and revision"
        );
        db.finish().await;
    }
}
