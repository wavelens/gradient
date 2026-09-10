/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-instance aggregation of served NAR traffic into the `cache_metric`
//! minute bucket.
//!
//! Every served NAR used to run its own upsert on the `(cache, bucket_time)`
//! row, so concurrent downloads of one cache serialised on that row lock: 2.6M
//! calls at 14.6 ms mean, almost all of it lock wait (#644). A serve now adds
//! into an in-memory map and the flush pass writes each bucket's sums once, so
//! the row takes one write per cache per interval per instance. A flush that
//! fails drops that interval's numbers: the accumulator is telemetry and must
//! never hold a serve up or grow without bound.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDateTime, Timelike};
use gradient_entity::ids::CacheId;
use gradient_util::supervision::ChildSpec;
use gradient_util::sync::Mutex;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use tracing::warn;
use uuid::Uuid;

use super::DbContext;

/// Bytes and NAR count accumulated for one `(cache, bucket)` pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    pub bytes: i64,
    pub nars: i64,
}

#[derive(Debug, Default)]
pub struct CacheTraffic {
    buckets: Mutex<HashMap<(CacheId, NaiveDateTime), Traffic>>,
}

impl CacheTraffic {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Add one served NAR of `bytes` to `bucket`. Never fails and never waits
    /// on the database, so it is safe on the request path.
    pub fn record(&self, cache: CacheId, bucket: NaiveDateTime, bytes: i64) {
        let mut buckets = self.buckets.lock();
        let entry = buckets.entry((cache, bucket)).or_default();
        entry.bytes = entry.bytes.saturating_add(bytes);
        entry.nars = entry.nars.saturating_add(1);
    }

    /// Take everything accumulated so far. Serves landing after this call go
    /// into the next interval's map.
    pub fn take(&self) -> Vec<(CacheId, NaiveDateTime, Traffic)> {
        let mut buckets = self.buckets.lock();
        buckets
            .drain()
            .map(|((cache, bucket), traffic)| (cache, bucket, traffic))
            .collect()
    }
}

/// The minute bucket a serve at `at` belongs to.
pub fn minute_bucket(at: NaiveDateTime) -> NaiveDateTime {
    at.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(at)
}

/// Write every accumulated bucket. Concurrent flushes from several instances
/// add into the same row, which the additive upsert makes safe.
pub async fn flush(ctx: &DbContext, traffic: &CacheTraffic) {
    for (cache, bucket, sample) in traffic.take() {
        if let Err(e) = ctx
            .worker_db
            .execute_raw(add_traffic_stmt(cache, bucket, sample))
            .await
        {
            warn!(error = %e, %cache, "cache metric flush failed");
        }
    }
}

/// The flush pass as a supervised child.
pub fn child_spec(ctx: DbContext, traffic: Arc<CacheTraffic>) -> ChildSpec {
    let secs = ctx
        .config
        .metrics_args
        .cache_metric_flush_interval_secs
        .max(1);

    ChildSpec::periodic(
        "cache_metric_flush",
        Duration::from_secs(secs),
        Duration::from_secs(60),
        move || {
            let ctx = ctx.clone();
            let traffic = Arc::clone(&traffic);

            async move {
                flush(&ctx, &traffic).await;
                Ok(())
            }
        },
    )
}

/// A supervised pass is cancelled by the shutdown token before it can run, so
/// the last interval is written by a tracked task that wakes on that token and
/// is waited for by the drain.
pub fn flush_on_shutdown(ctx: DbContext, traffic: Arc<CacheTraffic>) {
    let shutdown = ctx.shutdown.clone();

    shutdown.spawn(async move {
        ctx.shutdown.cancelled().await;
        flush(&ctx, &traffic).await;
    });
}

/// Add one instance's sums into the `(cache, bucket_time)` row. Additive, so
/// neither a concurrent instance's flush nor a later interval of this one's
/// loses what the row already holds.
fn add_traffic_stmt(cache: CacheId, bucket: NaiveDateTime, traffic: Traffic) -> Statement {
    Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO cache_metric (id, cache, bucket_time, bytes_sent, nar_count)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (cache, bucket_time)
           DO UPDATE SET bytes_sent = cache_metric.bytes_sent + EXCLUDED.bytes_sent,
                         nar_count  = cache_metric.nar_count  + EXCLUDED.nar_count"#,
        [
            sea_orm::Value::Uuid(Some(Uuid::now_v7())),
            sea_orm::Value::Uuid(Some(cache.into_inner())),
            sea_orm::Value::ChronoDateTime(Some(bucket)),
            sea_orm::Value::BigInt(Some(traffic.bytes)),
            sea_orm::Value::BigInt(Some(traffic.nars)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use sea_orm::{DatabaseBackend as Backend, MockDatabase, MockExecResult};

    fn bucket(minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 5, 2)
            .expect("date")
            .and_hms_opt(12, minute, 0)
            .expect("time")
    }

    fn cache(n: u128) -> CacheId {
        CacheId::new(Uuid::from_u128(n))
    }

    fn exec_results(n: usize) -> Vec<MockExecResult> {
        (0..n)
            .map(|_| MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
            .collect()
    }

    /// The point of #644: two serves of one cache inside one bucket are one
    /// statement, carrying both the summed bytes and the count.
    #[tokio::test]
    async fn two_serves_in_one_bucket_flush_as_one_upsert() {
        let db = MockDatabase::new(Backend::Postgres)
            .append_exec_results(exec_results(1))
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let traffic = CacheTraffic::new();

        traffic.record(cache(1), bucket(34), 4096);
        traffic.record(cache(1), bucket(34), 1024);
        flush(&ctx, &traffic).await;

        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 1, "one upsert for both serves: {log:?}");
        assert!(log[0].contains("INSERT INTO cache_metric"), "{log:?}");
        assert!(
            log[0].contains("ON CONFLICT (cache, bucket_time)"),
            "{log:?}"
        );
        assert!(
            log[0].contains("bytes_sent = cache_metric.bytes_sent + EXCLUDED.bytes_sent"),
            "the row must be added into, not overwritten: {log:?}"
        );
        assert!(
            log[0].contains("nar_count  = cache_metric.nar_count  + EXCLUDED.nar_count"),
            "{log:?}"
        );
        assert!(log[0].contains("5120"), "the bytes are summed: {log:?}");
        assert!(!log[0].to_uppercase().contains("SELECT"), "{log:?}");
    }

    #[tokio::test]
    async fn an_idle_interval_writes_nothing() {
        let db = MockDatabase::new(Backend::Postgres).into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        flush(&ctx, &CacheTraffic::new()).await;

        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(log.is_empty(), "no serves, no statement: {log:?}");
    }

    #[tokio::test]
    async fn each_cache_and_bucket_is_its_own_row() {
        let db = MockDatabase::new(Backend::Postgres)
            .append_exec_results(exec_results(3))
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let traffic = CacheTraffic::new();

        traffic.record(cache(1), bucket(34), 1);
        traffic.record(cache(2), bucket(34), 1);
        traffic.record(cache(1), bucket(35), 1);
        flush(&ctx, &traffic).await;

        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 3, "two caches over two buckets: {log:?}");
    }

    #[test]
    fn a_flush_clears_what_it_took() {
        let traffic = CacheTraffic::new();
        traffic.record(cache(1), bucket(34), 7);

        assert_eq!(
            traffic.take(),
            vec![(cache(1), bucket(34), Traffic { bytes: 7, nars: 1 })]
        );
        assert!(traffic.take().is_empty(), "a second flush has nothing left");
    }

    #[test]
    fn a_serve_is_bucketed_by_its_minute() {
        let at = NaiveDate::from_ymd_opt(2026, 5, 2)
            .expect("date")
            .and_hms_milli_opt(12, 34, 56, 789)
            .expect("time");

        assert_eq!(minute_bucket(at), bucket(34));
    }
}
