/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::ids::CacheId;
use gradient_entity::metric_rollup::RollupGranularity;
use gradient_util::telemetry::metric;
use sea_orm::{ConnectionTrait, DbErr, Value};

fn infra_series_sql(window_hours: i64) -> String {
    format!(
        "SELECT bucket_start, sum(count)::bigint AS c, sum(sum) AS s \
         FROM metric_rollup WHERE metric = $1 AND granularity = {gran} \
           AND bucket_start >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours' \
         GROUP BY bucket_start ORDER BY bucket_start",
        gran = i16::from(RollupGranularity::Hour),
    )
}

crate::sql_fn! {
    INFRA_SERIES = || infra_series_sql(24),
        params = [Text(metric::CACHE_BYTES_SENT)];
}

pub struct BucketTotal {
    pub bucket_start: NaiveDateTime,
    pub count: i64,
    pub sum: f64,
}

pub async fn hourly_totals<C: ConnectionTrait>(
    db: &C,
    metric: &str,
    window_hours: i64,
) -> Result<Vec<BucketTotal>, DbErr> {
    let rows = db
        .query_all_raw(INFRA_SERIES.bind_built(infra_series_sql(window_hours), [metric.into()]))
        .await?;

    Ok(rows
        .iter()
        .map(|r| BucketTotal {
            bucket_start: r.try_get("", "bucket_start").unwrap_or_default(),
            count: r.try_get("", "c").unwrap_or(0),
            sum: r.try_get("", "s").unwrap_or(0.0),
        })
        .collect())
}

fn board_upstream_caches_sql(window_hours: i64) -> String {
    format!(
        "SELECT mr.scope->>'upstream_url' AS upstream_url, mr.metric AS metric, \
                mr.bucket_start AS bucket_start, mr.count AS c, mr.sum AS s \
         FROM metric_rollup mr \
         WHERE mr.metric IN ('upstream.latency_ms','upstream.narinfo_hits','upstream.narinfo_misses') \
           AND mr.granularity = {gran} \
           AND mr.bucket_start >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours' \
         ORDER BY mr.bucket_start",
        gran = i16::from(RollupGranularity::Hour),
    )
}

crate::sql_fn! {
    BOARD_UPSTREAM_CACHES = || board_upstream_caches_sql(24),
        params = [];
}

pub struct UpstreamCacheBucket {
    pub upstream_url: Option<String>,
    pub metric: String,
    pub bucket_start: NaiveDateTime,
    pub count: i64,
    pub sum: f64,
}

pub async fn upstream_cache_buckets<C: ConnectionTrait>(
    db: &C,
    window_hours: i64,
) -> Result<Vec<UpstreamCacheBucket>, DbErr> {
    let rows = db
        .query_all_raw(
            BOARD_UPSTREAM_CACHES.bind_built(board_upstream_caches_sql(window_hours), []),
        )
        .await?;

    Ok(rows
        .iter()
        .map(|r| UpstreamCacheBucket {
            upstream_url: r.try_get("", "upstream_url").ok(),
            metric: r.try_get("", "metric").unwrap_or_default(),
            bucket_start: r.try_get("", "bucket_start").unwrap_or_default(),
            count: r.try_get("", "c").unwrap_or(0),
            sum: r.try_get("", "s").unwrap_or(0.0),
        })
        .collect())
}

fn latest_rollup_bucket_sql() -> String {
    format!(
        "SELECT max(bucket_start) AS m FROM metric_rollup WHERE granularity = {}",
        i16::from(RollupGranularity::Minute)
    )
}

crate::sql_fn! {
    LATEST_ROLLUP_BUCKET = latest_rollup_bucket_sql,
        params = [];
}

pub async fn latest_minute_bucket<C: ConnectionTrait>(
    db: &C,
) -> Result<Option<NaiveDateTime>, DbErr> {
    Ok(db
        .query_one_raw(LATEST_ROLLUP_BUCKET.stmt())
        .await?
        .and_then(|r| r.try_get::<Option<NaiveDateTime>>("", "m").ok().flatten()))
}

fn labelled_series_sql(granularity: RollupGranularity, window_hours: i64) -> String {
    format!(
        "SELECT bucket_start, COALESCE(scope->>'label', '') AS label, \
                sum(count)::bigint AS c, sum(sum) AS s, max(max) AS m \
         FROM metric_rollup WHERE metric = $1 AND granularity = {gran} \
           AND bucket_start >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours' \
         GROUP BY 1, 2 ORDER BY 2, 1",
        gran = i16::from(granularity),
    )
}

crate::sql_fn! {
    LABELLED_SERIES = || labelled_series_sql(RollupGranularity::Minute, 6),
        params = [Text(metric::STORAGE_OP_MS)];
}

pub struct LabelledBucket {
    pub bucket_start: NaiveDateTime,
    pub label: String,
    pub count: i64,
    pub sum: f64,
    pub max: f64,
}

pub async fn labelled<C: ConnectionTrait>(
    db: &C,
    metric: &str,
    granularity: RollupGranularity,
    window_hours: i64,
) -> Result<Vec<LabelledBucket>, DbErr> {
    let sql = labelled_series_sql(granularity, window_hours);
    let rows = db
        .query_all_raw(LABELLED_SERIES.bind_built(sql, [metric.into()]))
        .await?;

    Ok(rows
        .iter()
        .map(|r| LabelledBucket {
            bucket_start: r.try_get("", "bucket_start").unwrap_or_default(),
            label: r.try_get("", "label").unwrap_or_default(),
            count: r.try_get("", "c").unwrap_or(0),
            sum: r.try_get("", "s").unwrap_or(0.0),
            max: r.try_get("", "m").unwrap_or(0.0),
        })
        .collect())
}

fn cache_series_sql(unit: &str, back: &str, gran: i16) -> String {
    format!(
        r#"SELECT gs.period,
                  COALESCE(SUM(mr.count), 0)::bigint AS cnt,
                  COALESCE(SUM(mr.sum), 0)::bigint    AS total
           FROM generate_series(
               date_trunc('{unit}', NOW() AT TIME ZONE 'UTC') - INTERVAL '{back}',
               date_trunc('{unit}', NOW() AT TIME ZONE 'UTC'),
               INTERVAL '1 {unit}'
           ) AS gs(period)
           LEFT JOIN metric_rollup mr
               ON mr.bucket_start = gs.period
              AND mr.metric = $1
              AND mr.granularity = {gran}
              AND (mr.scope->>'cache') = $2
           GROUP BY gs.period
           ORDER BY gs.period"#,
    )
}

crate::sql_fn! {
    CACHE_SERIES = || cache_series_sql("hour", "23 hours", 1),
        params = [Text(metric::CACHE_BYTES_SENT), Text("11111111-1111-1111-1111-111111111111")];
}

pub async fn of_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    metric: &str,
    granularity: RollupGranularity,
    back_interval: &str,
) -> Result<Vec<(NaiveDateTime, i64, i64)>, DbErr> {
    let sql = cache_series_sql(
        granularity.trunc_unit(),
        back_interval,
        i16::from(granularity),
    );

    let rows = db
        .query_all_raw(CACHE_SERIES.bind_built(
            sql,
            [
                Value::String(Some(metric.to_owned())),
                Value::String(Some(cache.to_string())),
            ],
        ))
        .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let time: NaiveDateTime = row.try_get("", "period").ok()?;
            let cnt: i64 = row.try_get("", "cnt").unwrap_or(0);
            let total: i64 = row.try_get("", "total").unwrap_or(0);
            Some((time, cnt, total))
        })
        .collect())
}

fn metrics_query_sql(project_filter: Option<&str>, has_from: bool, has_to: bool) -> String {
    let mut sql = String::from(
        "SELECT bucket_start, sum(count)::bigint AS c, sum(sum) AS s, \
                min(min) AS mn, max(max) AS mx \
         FROM metric_rollup WHERE metric = $1 AND granularity = $2",
    );

    if let Some(list) = project_filter {
        // DB-sourced UUID strings are safe to inline as a quoted IN list.
        sql.push_str(&format!(" AND (scope->>'project') IN ({list})"));
    }

    let mut n = 2;
    if has_from {
        n += 1;
        sql.push_str(&format!(" AND bucket_start >= ${n}"));
    }

    if has_to {
        n += 1;
        sql.push_str(&format!(" AND bucket_start <= ${n}"));
    }

    sql.push_str(" GROUP BY bucket_start ORDER BY bucket_start");
    sql
}

crate::sql_fn! {
    METRICS_QUERY = || metrics_query_sql(
        Some("'11111111-1111-1111-1111-111111111111'"),
        true,
        true,
    ),
        params = [Text(metric::BUILDS_CREATED), Int(1), Now, Now];
}

pub struct BucketStats {
    pub bucket_start: NaiveDateTime,
    pub count: i64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

pub async fn of_projects<C: ConnectionTrait>(
    db: &C,
    metric: &str,
    granularity: RollupGranularity,
    project_filter: Option<&str>,
    from: Option<NaiveDateTime>,
    to: Option<NaiveDateTime>,
) -> Result<Vec<BucketStats>, DbErr> {
    let mut values: Vec<Value> = vec![
        Value::from(metric.to_owned()),
        Value::from(i16::from(granularity)),
    ];
    if let Some(from) = from {
        values.push(Value::from(from));
    }

    if let Some(to) = to {
        values.push(Value::from(to));
    }

    let rows = db
        .query_all_raw(METRICS_QUERY.bind_built(
            metrics_query_sql(project_filter, from.is_some(), to.is_some()),
            values,
        ))
        .await?;

    Ok(rows
        .iter()
        .map(|r| BucketStats {
            bucket_start: r.try_get("", "bucket_start").unwrap_or_default(),
            count: r.try_get("", "c").unwrap_or(0),
            sum: r.try_get("", "s").unwrap_or(0.0),
            min: r.try_get("", "mn").unwrap_or(0.0),
            max: r.try_get("", "mx").unwrap_or(0.0),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_group_by_label_within_the_window() {
        let sql = labelled_series_sql(RollupGranularity::Minute, 6);
        assert!(sql.contains("metric = $1"), "{sql}");
        assert!(sql.contains("granularity = 0"), "{sql}");
        assert!(sql.contains("interval '6 hours'"), "{sql}");
        assert!(sql.contains("COALESCE(scope->>'label', '')"), "{sql}");
        assert!(sql.contains("GROUP BY 1, 2"), "{sql}");
        assert!(sql.contains("max(max)"), "{sql}");
    }
}
