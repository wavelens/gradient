/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The flush is additive, keeping several instances writing one minute exact.

use std::time::Duration;

use chrono::DateTime;
use gradient_util::supervision::ChildSpec;
use gradient_util::telemetry::{Agg, GAUGES, Gauges, Key, MinuteStats, STATS, metric};
use sea_orm::{ConnectionTrait, Statement};
use tracing::warn;
use uuid::Uuid;

use crate::DbContext;

pub async fn flush(ctx: &DbContext, stats: &MinuteStats, gauges: &Gauges) {
    stats.sample(gauges);

    for (key, agg) in stats.take() {
        if let Err(e) = ctx.worker_db.execute_raw(add_minute_stmt(&key, agg)).await {
            warn!(error = %e, metric = key.metric, "infra metric flush failed");
        }
    }
}

pub fn child_spec(ctx: DbContext) -> ChildSpec {
    let secs = ctx.config.metrics_args.cache_flush_interval_secs.max(1);

    ChildSpec::periodic(
        "infra_metric_flush",
        Duration::from_secs(secs),
        Duration::from_secs(60),
        move || {
            let ctx = ctx.clone();

            async move {
                flush(&ctx, &STATS, &GAUGES).await;
                Ok(())
            }
        },
    )
}

pub fn flush_on_shutdown(ctx: DbContext) {
    let shutdown = ctx.shutdown.clone();

    shutdown.spawn(async move {
        ctx.shutdown.cancelled().await;
        flush(&ctx, &STATS, &GAUGES).await;
    });
}

fn scope_json(label: &str) -> String {
    if label.is_empty() {
        return "{}".to_owned();
    }

    serde_json::json!({ "label": label }).to_string()
}

crate::sql! {
    ADD_MINUTE = r#"INSERT INTO metric_rollup
           (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram)
           VALUES ($1, $2, 0, $3, $4::jsonb, hashtextextended($4::text, 0), $5, $6::float8, $7::float8, $8::float8, $9::float8, NULL)
           ON CONFLICT (metric, granularity, bucket_start, scope_hash)
           DO UPDATE SET count = metric_rollup.count + EXCLUDED.count,
                         sum = metric_rollup.sum + EXCLUDED.sum,
                         min = LEAST(metric_rollup.min, EXCLUDED.min),
                         max = GREATEST(metric_rollup.max, EXCLUDED.max),
                         sum_sq = metric_rollup.sum_sq + EXCLUDED.sum_sq"#,
        params = [NewUuid, Text(metric::STORAGE_OP_MS), Now, Text("{}"), Int(1), Int(1), Int(1), Int(1), Int(1)];
}

fn add_minute_stmt(key: &Key, agg: Agg) -> Statement {
    let bucket = DateTime::from_timestamp(key.minute, 0)
        .unwrap_or_default()
        .naive_utc();

    ADD_MINUTE.bind([
        sea_orm::Value::Uuid(Some(Uuid::now_v7())),
        sea_orm::Value::String(Some(key.metric.to_owned())),
        sea_orm::Value::ChronoDateTime(Some(bucket)),
        sea_orm::Value::String(Some(scope_json(&key.label))),
        sea_orm::Value::BigInt(Some(agg.count)),
        sea_orm::Value::Double(Some(agg.sum)),
        sea_orm::Value::Double(Some(agg.min)),
        sea_orm::Value::Double(Some(agg.max)),
        sea_orm::Value::Double(Some(agg.sum_sq)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend as Backend, MockDatabase, MockExecResult};

    fn exec_results(n: usize) -> Vec<MockExecResult> {
        (0..n)
            .map(|_| MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_minute_is_added_into_its_rollup_row() {
        let db = MockDatabase::new(Backend::Postgres)
            .append_exec_results(exec_results(5))
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let stats = MinuteStats::default();
        stats.record_at(metric::STORAGE_OP_MS, "get", 12.5, 1_790_000_040);

        flush(&ctx, &stats, &Gauges::new()).await;

        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        let upsert = log
            .iter()
            .find(|s| s.contains("storage.op_ms"))
            .expect("storage.op_ms row");
        assert!(upsert.contains("INSERT INTO metric_rollup"), "{upsert}");
        assert!(
            upsert.contains("ON CONFLICT (metric, granularity, bucket_start, scope_hash)"),
            "{upsert}"
        );
        assert!(
            upsert.contains("count = metric_rollup.count + EXCLUDED.count"),
            "{upsert}"
        );
        assert!(
            upsert.contains("LEAST(metric_rollup.min, EXCLUDED.min)"),
            "{upsert}"
        );
        assert!(
            upsert.contains("GREATEST(metric_rollup.max, EXCLUDED.max)"),
            "{upsert}"
        );
        assert!(
            upsert.contains(r#"{\"label\":\"get\"}"#),
            "scope carries the label: {upsert}"
        );
    }

    #[tokio::test]
    async fn gauges_are_sampled_on_every_flush() {
        let db = MockDatabase::new(Backend::Postgres)
            .append_exec_results(exec_results(4))
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        flush(&ctx, &MinuteStats::default(), &Gauges::new()).await;

        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 4, "four gauge rows even when idle: {log:?}");
        for m in [
            metric::PROTO_BULK_LANE_FILL,
            metric::PROTO_CONTROL_LANE_FILL,
            metric::NAR_SERVES_WAITING,
            metric::NAR_SERVES_ACTIVE,
        ] {
            assert!(log.iter().any(|s| s.contains(m)), "{m} missing: {log:?}");
        }
    }

    #[test]
    fn scope_is_empty_without_a_label() {
        assert_eq!(scope_json(""), "{}");
        assert_eq!(scope_json("get/cancelled"), r#"{"label":"get/cancelled"}"#);
    }
}
