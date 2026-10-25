/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_entity::cache::Model as MCache;
use gradient_entity::cache_upstream::{
    CacheUpstreamKind, Column as CCacheUpstream, Entity as ECacheUpstream, Model as MCacheUpstream,
};
use gradient_entity::project_cache::{
    CacheSubscriptionMode, Column as CProjectCache, Entity as EProjectCache,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use gradient_types::ids::{CacheId, CacheUpstreamId, EvaluationId, ProjectId};

#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamEndpoint {
    pub id: CacheUpstreamId,
    pub url: String,
    pub public_key: Option<String>,
    pub http1_only: bool,
    pub avg_latency_ms: Option<f64>,
    pub hit_rate: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct UpstreamAccum {
    pub latency_ms_sum: f64,
    pub request_count: i64,
    pub narinfo_hits: i64,
    pub narinfo_misses: i64,
}

impl UpstreamAccum {
    pub fn record_hit(&mut self, latency_ms: f64) {
        self.latency_ms_sum += latency_ms;
        self.request_count += 1;
        self.narinfo_hits += 1;
    }

    pub fn record_miss(&mut self, latency_ms: f64) {
        self.latency_ms_sum += latency_ms;
        self.request_count += 1;
        self.narinfo_misses += 1;
    }

    pub fn record_error(&mut self, latency_ms: f64) {
        self.latency_ms_sum += latency_ms;
        self.request_count += 1;
    }

    pub fn merge(&mut self, other: &UpstreamAccum) {
        self.latency_ms_sum += other.latency_ms_sum;
        self.request_count += other.request_count;
        self.narinfo_hits += other.narinfo_hits;
        self.narinfo_misses += other.narinfo_misses;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradientProtoUpstream {
    pub url: String,
    pub remote_cache: String,
    pub public_key: Option<String>,
    pub api_key_enc: Option<String>,
}

pub async fn pull_through_upstream_caches<C: ConnectionTrait>(
    db: &C,
    cache: &MCache,
) -> Result<Vec<MCacheUpstream>, DbErr> {
    if !cache.pull_through {
        return Ok(Vec::new());
    }

    ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .filter(CCacheUpstream::Active.eq(true))
        .all(db)
        .await
}

pub async fn gradient_proto_upstream_caches_for_project<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
) -> Result<Vec<GradientProtoUpstream>> {
    let project_cache_rows = EProjectCache::find()
        .filter(
            sea_orm::Condition::all()
                .add(CProjectCache::Project.eq(project_id))
                .add(CProjectCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    let cache_ids: Vec<CacheId> = project_cache_rows.iter().map(|r| r.cache).collect();
    if cache_ids.is_empty() {
        return Ok(Vec::new());
    }

    let upstream_rows = ECacheUpstream::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCacheUpstream::Cache.is_in(cache_ids))
                .add(CCacheUpstream::Kind.eq(CacheUpstreamKind::GradientProto))
                .add(CCacheUpstream::Active.eq(true))
                .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    Ok(upstream_rows
        .into_iter()
        .filter_map(|r| {
            Some(GradientProtoUpstream {
                url: r.url?,
                remote_cache: r.remote_cache_name?,
                public_key: r.public_key,
                api_key_enc: r.api_key,
            })
        })
        .collect())
}

fn upstream_endpoints_sql(window_minutes: i64) -> String {
    format!(
        "SELECT cu.id AS id, cu.url AS url, cu.public_key AS public_key, cu.http1_only AS http1_only, \
                SUM(um.latency_ms_sum) / NULLIF(SUM(um.request_count), 0) AS avg_latency_ms, \
                SUM(um.narinfo_hits)::float8 \
                  / NULLIF(SUM(um.narinfo_hits + um.narinfo_misses), 0) AS hit_rate \
         FROM cache_upstream cu \
         JOIN project_cache oc ON oc.cache = cu.cache \
         LEFT JOIN upstream_metric um ON um.upstream_url = cu.url \
              AND um.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '{window_minutes} minutes' \
         WHERE oc.project = $1 AND oc.mode <> 2 AND cu.kind = 2 \
               AND cu.mode <> 2 AND cu.active AND cu.url IS NOT NULL \
         GROUP BY cu.id, cu.url, cu.public_key, cu.http1_only",
        window_minutes = window_minutes
    )
}

crate::sql_fn! {
    UPSTREAM_ENDPOINTS_FOR_PROJECT = || upstream_endpoints_sql(60),
        params = [ProjectId];
}

pub async fn upstream_endpoints_for_project<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
    window_minutes: i64,
) -> Result<Vec<UpstreamEndpoint>> {
    // `window_minutes` is baked into the text instead of bound.
    // The exemplar above is then the shape the plan gate is checking.
    let rows = db
        .query_all_raw(UPSTREAM_ENDPOINTS_FOR_PROJECT.bind_built(
            upstream_endpoints_sql(window_minutes),
            [project_id.into_inner().into()],
        ))
        .await?;

    let endpoints = rows
        .into_iter()
        .filter_map(|r| {
            let id: uuid::Uuid = r.try_get("", "id").ok()?;
            let url: String = r.try_get("", "url").ok()?;
            Some(UpstreamEndpoint {
                id: CacheUpstreamId::new(id),
                url,
                public_key: r.try_get("", "public_key").ok().flatten(),
                http1_only: r.try_get("", "http1_only").unwrap_or(false),
                avg_latency_ms: r.try_get("", "avg_latency_ms").ok(),
                hit_rate: r.try_get("", "hit_rate").ok(),
            })
        })
        .collect();

    Ok(endpoints)
}

crate::sql! {
    FAILING_UPSTREAMS_FOR_EVALUATION = "SELECT cu.url AS url \
         FROM cache_upstream cu \
         JOIN project_cache oc ON oc.cache = cu.cache \
         JOIN upstream_metric um ON um.upstream_url = cu.url \
              AND um.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '5 minutes' \
         WHERE oc.project = (SELECT t.project FROM evaluation e JOIN task t ON t.id = e.task WHERE e.id = $1) \
               AND oc.mode <> 2 AND cu.kind = 2 AND cu.mode <> 2 AND cu.active AND cu.url IS NOT NULL \
         GROUP BY cu.url \
         HAVING SUM(um.request_count) > SUM(um.narinfo_hits + um.narinfo_misses) \
         ORDER BY cu.url",
        params = [EvaluationId];
}

pub async fn failing_upstreams_for_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<String>> {
    Ok(db
        .query_all_raw(FAILING_UPSTREAMS_FOR_EVALUATION.bind([evaluation.into_inner().into()]))
        .await?
        .into_iter()
        .filter_map(|r| r.try_get("", "url").ok())
        .collect())
}

crate::sql! {
    UPSERT_UPSTREAM_METRIC = "INSERT INTO upstream_metric \
                 (id, upstream_url, bucket_time, latency_ms_sum, request_count, narinfo_hits, narinfo_misses) \
             VALUES (uuidv7(), $1, $2, $3, $4, $5, $6) \
             ON CONFLICT (upstream_url, bucket_time) DO UPDATE SET \
                 latency_ms_sum = upstream_metric.latency_ms_sum + EXCLUDED.latency_ms_sum, \
                 request_count  = upstream_metric.request_count  + EXCLUDED.request_count, \
                 narinfo_hits   = upstream_metric.narinfo_hits   + EXCLUDED.narinfo_hits, \
                 narinfo_misses = upstream_metric.narinfo_misses + EXCLUDED.narinfo_misses",
        params = [Text("https://cache.example/"), Now, Int(120), Int(4), Int(3), Int(1)];
}

pub async fn upsert_upstream_metrics<C: ConnectionTrait>(
    db: &C,
    bucket: chrono::NaiveDateTime,
    accum: &std::collections::HashMap<String, UpstreamAccum>,
) -> Result<()> {
    let to_column = |count: i64| i32::try_from(count).unwrap_or(i32::MAX);
    for (url, a) in accum {
        if a.request_count == 0 {
            continue;
        }

        db.execute_raw(UPSERT_UPSTREAM_METRIC.bind([
            url.clone().into(),
            bucket.into(),
            a.latency_ms_sum.into(),
            to_column(a.request_count).into(),
            to_column(a.narinfo_hits).into(),
            to_column(a.narinfo_misses).into(),
        ]))
        .await?;
    }

    Ok(())
}

fn upstream_urls_for_projects_sql(project_list: &str) -> String {
    format!(
        "SELECT DISTINCT cu.url AS url FROM cache_upstream cu \
         JOIN project_cache oc ON oc.cache = cu.cache \
         WHERE oc.project IN ({project_list}) AND cu.url IS NOT NULL"
    )
}

crate::sql_fn! {
    UPSTREAM_URLS_FOR_PROJECTS = || {
        upstream_urls_for_projects_sql("'11111111-1111-1111-1111-111111111111'")
    },
        params = [];
}

pub async fn upstream_urls_for_projects<C: ConnectionTrait>(
    db: &C,
    project_list: &str,
) -> Result<std::collections::HashSet<String>> {
    Ok(db
        .query_all_raw(
            UPSTREAM_URLS_FOR_PROJECTS.bind_built(upstream_urls_for_projects_sql(project_list), []),
        )
        .await?
        .into_iter()
        .filter_map(|r| r.try_get::<String>("", "url").ok())
        .collect())
}

pub async fn of_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<MCacheUpstream>, DbErr> {
    ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache))
        .all(db)
        .await
}

pub async fn in_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    upstream: CacheUpstreamId,
) -> Result<Option<MCacheUpstream>, DbErr> {
    ECacheUpstream::find_by_id(upstream)
        .filter(CCacheUpstream::Cache.eq(cache))
        .one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::DatabaseConnection;

    fn upstream_row() -> MCacheUpstream {
        MCacheUpstream {
            url: Some("https://cache.nixos.org".into()),
            ..Default::default()
        }
    }

    fn db_with_an_active_upstream_cache() -> DatabaseConnection {
        mock_db()
            .append_query_results([vec![upstream_row()]])
            .into_connection()
    }

    #[tokio::test]
    async fn a_pull_through_cache_asks_its_active_upstream_caches() {
        let cache = MCache::default();

        let upstream_caches =
            pull_through_upstream_caches(&db_with_an_active_upstream_cache(), &cache)
                .await
                .unwrap();

        assert_eq!(upstream_caches, vec![upstream_row()]);
    }

    #[tokio::test]
    async fn a_cache_with_pull_through_off_asks_no_upstream_cache() {
        let cache = MCache {
            pull_through: false,
            ..Default::default()
        };

        let upstream_caches =
            pull_through_upstream_caches(&db_with_an_active_upstream_cache(), &cache)
                .await
                .unwrap();

        assert!(upstream_caches.is_empty());
    }
}
