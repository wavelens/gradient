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
               AND kind IN (2, 3, 4, 5) \
             ORDER BY next_attempt_at, id LIMIT $1 FOR UPDATE SKIP LOCKED) \
         RETURNING o.id, o.kind, o.key, o.payload, o.attempts",
        params = [Int(8), Int(CLAIM_LEASE_SECS)];

    PENDING_DELIVERY_MARK_DELIVERED = "UPDATE pending_delivery SET delivered_at = (now() AT TIME ZONE 'UTC') WHERE id = $1",
        params = [NewUuid];

    /// The attempt count, the dead-letter decision and the next attempt are moving together.
    /// A read of `attempts` followed by a write would lose a concurrent retry of the same row.
    PENDING_DELIVERY_MARK_RETRY = "UPDATE pending_delivery SET attempts = attempts + 1, last_error = $2, \
         failed_at = CASE WHEN attempts + 1 >= $3 THEN (now() AT TIME ZONE 'UTC') ELSE NULL END, \
         next_attempt_at = (now() AT TIME ZONE 'UTC') + make_interval(secs => $4::int) \
         WHERE id = $1",
        params = [NewUuid, Text("probe"), Int(MAX_ATTEMPTS as i64), Int(30)];

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    Retry(String),
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

/// Keys are sorted to give overlapping concurrent batches the same lock order.
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

    let ids: Vec<uuid::Uuid> = rows
        .iter()
        .map(|_| PendingDeliveryId::now_v7().into_inner())
        .collect();
    let keys: Vec<String> = rows.iter().map(|(key, _)| key.clone()).collect();
    let payloads: Vec<String> = rows.iter().map(|(_, p)| p.to_string()).collect();

    db.execute_raw(PENDING_DELIVERY_ENQUEUE_MANY.bind([
        ids.into(),
        Value::SmallInt(Some(i16::from(kind))),
        keys.into(),
        payloads.into(),
    ]))
    .await?;

    Ok(())
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
            })
        })
        .collect()
}

pub async fn mark<C: ConnectionTrait>(
    db: &C,
    row: &PendingDelivery,
    outcome: &Outcome,
) -> Result<(), DbErr> {
    let stmt = match outcome {
        Outcome::Delivered => {
            PENDING_DELIVERY_MARK_DELIVERED.bind([Value::Uuid(Some(row.id.into_inner()))])
        }
        Outcome::Retry(error) => PENDING_DELIVERY_MARK_RETRY.bind([
            Value::Uuid(Some(row.id.into_inner())),
            Value::String(Some(error.chars().take(4000).collect())),
            Value::Int(Some(MAX_ATTEMPTS)),
            Value::BigInt(Some(backoff(row.attempts).as_secs() as i64)),
        ]),
    };
    db.execute_raw(stmt).await?;

    Ok(())
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

    #[tokio::test]
    async fn claim_due_leases_in_one_skip_locked_statement() {
        let id = PendingDeliveryId::now_v7();
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
        assert_eq!(rows[0].kind, PendingDeliveryKind::ActionDelivery);
        let sql = norm(&db.into_transaction_log()[0].statements()[0].sql);
        assert!(
            sql.starts_with(
                "UPDATE pending_delivery o SET next_attempt_at = (now() AT TIME ZONE 'UTC') + make_interval(secs => $2::int)"
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

    #[test]
    fn the_claim_names_exactly_the_known_kinds() {
        use sea_orm::Iterable;

        let known: Vec<String> = PendingDeliveryKind::iter()
            .map(|k| i16::from(k).to_string())
            .collect();
        let sql = norm(&PENDING_DELIVERY_CLAIM_DUE.text());
        assert!(
            sql.contains(&format!("AND kind IN ({})", known.join(", "))),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn claim_due_asks_for_nothing_when_there_is_no_room() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        assert!(claim_due(&db, 0).await.unwrap().is_empty());
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn mark_retry_schedules_and_dead_letters_at_the_cap() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let row = PendingDelivery {
            id: PendingDeliveryId::now_v7(),
            kind: PendingDeliveryKind::ActionDelivery,
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
            PendingDeliveryKind::Event,
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
}
