/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The read is a separate statement and can run in parallel.
//! An `INSERT ... SELECT` is never running in parallel.

use std::time::Duration;

use gradient_types::ids::CacheId;
use gradient_util::supervision::{Pass, PassError};
use sea_orm::{ConnectionTrait, DbErr, Value};

use crate::DbContext;

const MINIMUM_CACHE_USAGE_RECOUNT_INTERVAL: Duration = Duration::from_secs(15 * 60);

crate::sql! {
    CACHE_USAGE_TOTALS = "SELECT s.cache AS cache, sum(cp.file_size)::bigint AS bytes \
        FROM cached_path_signature s JOIN cached_path cp ON cp.id = s.cached_path \
        GROUP BY s.cache",
        params = [],
        tier = Sweep;

    STORE_CACHE_USAGE = "INSERT INTO cache_usage (cache, bytes) \
        SELECT c.id, coalesce(x.bytes, 0) FROM cache c \
        LEFT JOIN unnest($1::uuid[], $2::bigint[]) AS x(cache, bytes) ON x.cache = c.id \
        ON CONFLICT (cache) DO UPDATE SET bytes = EXCLUDED.bytes \
        WHERE cache_usage.bytes IS DISTINCT FROM EXCLUDED.bytes",
        params = [CacheIds(8), Ints(0, 8)];
}

async fn cache_usage_totals<C: ConnectionTrait>(db: &C) -> Result<Vec<(CacheId, i64)>, DbErr> {
    db.query_all_raw(CACHE_USAGE_TOTALS.stmt())
        .await?
        .iter()
        .map(|r| Ok((r.try_get("", "cache")?, r.try_get("", "bytes")?)))
        .collect()
}

async fn store_cache_usage<C: ConnectionTrait>(
    db: &C,
    totals: Vec<(CacheId, i64)>,
) -> Result<u64, DbErr> {
    let (caches, bytes): (Vec<CacheId>, Vec<i64>) = totals.into_iter().unzip();
    let caches: Vec<uuid::Uuid> = caches.into_iter().map(CacheId::into_inner).collect();
    let stmt = STORE_CACHE_USAGE.bind([Value::from(caches), Value::from(bytes)]);
    Ok(db.execute_raw(stmt).await?.rows_affected())
}

pub async fn recount_cache_usage<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let totals = cache_usage_totals(db).await?;
    store_cache_usage(db, totals).await
}

fn cache_usage_recount_period_from_rollup_interval(rollup_interval: Duration) -> Duration {
    rollup_interval.max(MINIMUM_CACHE_USAGE_RECOUNT_INTERVAL)
}

pub struct CacheUsageRecount {
    ctx: DbContext,
    period: Duration,
}

impl CacheUsageRecount {
    pub fn new(ctx: DbContext) -> Self {
        let period = cache_usage_recount_period_from_rollup_interval(Duration::from_secs(
            ctx.config.metrics_args.rollup_interval_secs.get(),
        ));
        Self { ctx, period }
    }
}

impl Pass for CacheUsageRecount {
    const NAME: &'static str = "cache_usage_recount";
    const BUDGET: Duration = Duration::from_secs(180);
    const RUN_AT_START: bool = true;

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        recount_cache_usage(&self.ctx.worker_db).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::MockExecResult;
    use std::collections::BTreeMap;

    fn total(cache: CacheId, bytes: i64) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("cache".to_owned(), Value::from(cache.into_inner())),
            ("bytes".to_owned(), Value::from(bytes)),
        ])
    }

    #[tokio::test]
    async fn the_totals_read_is_written_back_as_parallel_arrays() {
        let (a, b) = (CacheId::now_v7(), CacheId::now_v7());
        let db = mock_db()
            .append_query_results([vec![total(a, 10), total(b, 20)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .into_connection();

        assert_eq!(recount_cache_usage(&db).await.unwrap(), 2);

        let log = db.into_transaction_log();
        let statements: Vec<_> = log.iter().flat_map(|t| t.statements()).collect();
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[0].sql, CACHE_USAGE_TOTALS.text());
        assert_eq!(statements[1].sql, STORE_CACHE_USAGE.text());
        let values = format!("{:?}", statements[1].values);
        assert!(values.contains(&a.into_inner().to_string()), "{values}");
        assert!(values.contains("BigInt(Some(20))"), "{values}");
    }

    #[test]
    fn the_cache_usage_recount_period_is_fifteen_minutes_at_the_default_rollup_interval_and_matches_an_admin_set_one_hour_interval()
     {
        assert_eq!(
            cache_usage_recount_period_from_rollup_interval(Duration::from_secs(60)),
            Duration::from_secs(15 * 60)
        );
        assert_eq!(
            cache_usage_recount_period_from_rollup_interval(Duration::from_secs(3600)),
            Duration::from_secs(3600)
        );
    }

    #[tokio::test]
    async fn an_emptied_cache_recounts_to_zero() {
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;

        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .into_cache(cache)
            .full()
            .into_database()
            .await;
        let usage = format!("SELECT bytes::text FROM cache_usage WHERE cache = '{cache}'");

        recount_cache_usage(&db.connection()).await.unwrap();
        assert_eq!(db.texts(&usage).await, vec![(1_i64 << 30).to_string()]);

        db.execute("DELETE FROM cached_path_signature").await;
        recount_cache_usage(&db.connection()).await.unwrap();
        assert_eq!(db.texts(&usage).await, vec!["0".to_owned()]);
        db.finish().await;
    }
}
