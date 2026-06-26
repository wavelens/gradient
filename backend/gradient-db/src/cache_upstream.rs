/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_entity::cache_upstream::{CacheUpstreamKind, Column as CCacheUpstream, Entity as ECacheUpstream};
use gradient_entity::organization_cache::{
    CacheSubscriptionMode, Column as COrganizationCache, Entity as EOrganizationCache,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement};

use gradient_types::ids::{CacheId, CacheUpstreamId, OrganizationId};

#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamEndpoint {
    pub id: CacheUpstreamId,
    pub url: String,
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
}

pub async fn upstream_urls_for_org<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
) -> Result<Vec<String>> {
    let org_cache_rows = EOrganizationCache::find()
        .filter(
            sea_orm::Condition::all()
                .add(COrganizationCache::Organization.eq(org_id))
                .add(COrganizationCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    let cache_ids: Vec<CacheId> = org_cache_rows.iter().map(|r| r.cache).collect();
    if cache_ids.is_empty() {
        return Ok(Vec::new());
    }

    let upstream_rows = ECacheUpstream::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCacheUpstream::Cache.is_in(cache_ids))
                .add(CCacheUpstream::Kind.eq(CacheUpstreamKind::Http))
                .add(CCacheUpstream::Url.is_not_null())
                .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    Ok(upstream_rows.into_iter().filter_map(|r| r.url).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradientProtoUpstream {
    pub url: String,
    pub remote_cache: String,
    pub public_key: Option<String>,
    pub api_key_enc: Option<String>,
}

pub async fn gradient_proto_upstreams_for_org<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
) -> Result<Vec<GradientProtoUpstream>> {
    let org_cache_rows = EOrganizationCache::find()
        .filter(
            sea_orm::Condition::all()
                .add(COrganizationCache::Organization.eq(org_id))
                .add(COrganizationCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    let cache_ids: Vec<CacheId> = org_cache_rows.iter().map(|r| r.cache).collect();
    if cache_ids.is_empty() {
        return Ok(Vec::new());
    }

    let upstream_rows = ECacheUpstream::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCacheUpstream::Cache.is_in(cache_ids))
                .add(CCacheUpstream::Kind.eq(CacheUpstreamKind::GradientProto))
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

pub async fn upstream_endpoints_for_org<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
    window_minutes: i64,
) -> Result<Vec<UpstreamEndpoint>> {
    let sql = format!(
        "SELECT cu.id AS id, cu.url AS url, \
                SUM(um.latency_ms_sum) / NULLIF(SUM(um.request_count), 0) AS avg_latency_ms, \
                SUM(um.narinfo_hits)::float8 \
                  / NULLIF(SUM(um.narinfo_hits + um.narinfo_misses), 0) AS hit_rate \
         FROM cache_upstream cu \
         JOIN organization_cache oc ON oc.cache = cu.cache \
         LEFT JOIN upstream_metric um ON um.upstream = cu.id \
              AND um.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '{window_minutes} minutes' \
         WHERE oc.organization = $1 AND oc.mode <> 2 AND cu.kind = 2 \
               AND cu.mode <> 2 AND cu.url IS NOT NULL \
         GROUP BY cu.id, cu.url",
        window_minutes = window_minutes
    );

    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [org_id.into_inner().into()],
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
                avg_latency_ms: r.try_get("", "avg_latency_ms").ok(),
                hit_rate: r.try_get("", "hit_rate").ok(),
            })
        })
        .collect();

    Ok(endpoints)
}

pub async fn upsert_upstream_metrics<C: ConnectionTrait>(
    db: &C,
    bucket: chrono::NaiveDateTime,
    accum: &std::collections::HashMap<CacheUpstreamId, UpstreamAccum>,
) -> Result<()> {
    for (upstream, a) in accum {
        if a.request_count == 0 {
            continue;
        }

        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO upstream_metric \
                 (id, upstream, bucket_time, latency_ms_sum, request_count, narinfo_hits, narinfo_misses) \
             VALUES (uuidv7(), $1, $2, $3, $4, $5, $6) \
             ON CONFLICT (upstream, bucket_time) DO UPDATE SET \
                 latency_ms_sum = upstream_metric.latency_ms_sum + EXCLUDED.latency_ms_sum, \
                 request_count  = upstream_metric.request_count  + EXCLUDED.request_count, \
                 narinfo_hits   = upstream_metric.narinfo_hits   + EXCLUDED.narinfo_hits, \
                 narinfo_misses = upstream_metric.narinfo_misses + EXCLUDED.narinfo_misses",
            [
                upstream.into_inner().into(),
                bucket.into(),
                a.latency_ms_sum.into(),
                (a.request_count as i32).into(),
                (a.narinfo_hits as i32).into(),
                (a.narinfo_misses as i32).into(),
            ],
        ))
        .await?;
    }

    Ok(())
}

pub async fn upstream_display_for_ids<C: ConnectionTrait>(
    db: &C,
    ids: Vec<String>,
) -> Result<std::collections::HashMap<String, (String, String)>> {
    use std::collections::HashMap;
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }

    let parsed: Vec<CacheUpstreamId> = ids
        .iter()
        .filter_map(|s| s.parse::<uuid::Uuid>().ok().map(CacheUpstreamId::new))
        .collect();

    if parsed.is_empty() {
        return Ok(out);
    }

    let rows = ECacheUpstream::find()
        .filter(CCacheUpstream::Id.is_in(parsed))
        .all(db)
        .await?;

    for r in rows {
        out.insert(r.id.to_string(), (r.display_name, r.url.unwrap_or_default()));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::cache_upstream::{self, CacheUpstreamKind};
    use gradient_entity::organization_cache::{self, CacheSubscriptionMode};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org_cache_row(
        org: OrganizationId,
        cache: CacheId,
        mode: CacheSubscriptionMode,
    ) -> organization_cache::Model {
        organization_cache::Model {
            id: gradient_types::ids::OrganizationCacheId::now_v7(),
            organization: org,
            cache,
            mode,
        }
    }

    fn upstream_row(cache: CacheId, kind: CacheUpstreamKind, url: Option<&str>) -> cache_upstream::Model {
        cache_upstream::Model {
            id: gradient_types::ids::CacheUpstreamId::now_v7(),
            cache,
            display_name: "test".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            kind,
            url: url.map(str::to_owned),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn returns_urls_from_subscribed_caches() {
        let org = OrganizationId::new(Uuid::now_v7());
        let cache_a = CacheId::new(Uuid::now_v7());
        let cache_b = CacheId::new(Uuid::now_v7());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                org_cache_row(org, cache_a, CacheSubscriptionMode::ReadOnly),
                org_cache_row(org, cache_b, CacheSubscriptionMode::ReadWrite),
            ]])
            .append_query_results([vec![
                upstream_row(cache_a, CacheUpstreamKind::Http, Some("https://cache-a.example/")),
                upstream_row(cache_b, CacheUpstreamKind::Http, Some("https://cache-b.example/")),
            ]])
            .into_connection();

        let urls = upstream_urls_for_org(&db, org)
            .await
            .expect("helper succeeds");
        assert_eq!(
            urls,
            vec![
                "https://cache-a.example/".to_string(),
                "https://cache-b.example/".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn empty_when_no_org_caches() {
        let org = OrganizationId::new(Uuid::now_v7());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<organization_cache::Model>::new()])
            .into_connection();

        let urls = upstream_urls_for_org(&db, org)
            .await
            .expect("helper succeeds");
        assert!(urls.is_empty());
    }

    #[tokio::test]
    async fn http_urls_excludes_gradient_proto() {
        let org = OrganizationId::new(Uuid::now_v7());
        let cache_a = CacheId::new(Uuid::now_v7());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![org_cache_row(org, cache_a, CacheSubscriptionMode::ReadOnly)]])
            .append_query_results([vec![
                upstream_row(cache_a, CacheUpstreamKind::Http, Some("https://http.example/")),
            ]])
            .into_connection();
        let urls = upstream_urls_for_org(&db, org).await.expect("ok");
        assert_eq!(urls, vec!["https://http.example/".to_string()]);
    }

    #[test]
    fn accum_record_hit_counts_hit_and_latency() {
        let mut a = UpstreamAccum::default();
        a.record_hit(12.0);
        a.record_hit(8.0);
        assert_eq!(a.request_count, 2);
        assert_eq!(a.narinfo_hits, 2);
        assert_eq!(a.narinfo_misses, 0);
        assert_eq!(a.latency_ms_sum, 20.0);
    }

    #[test]
    fn accum_record_miss_counts_miss_and_latency() {
        let mut a = UpstreamAccum::default();
        a.record_miss(5.0);
        assert_eq!(a.request_count, 1);
        assert_eq!(a.narinfo_hits, 0);
        assert_eq!(a.narinfo_misses, 1);
        assert_eq!(a.latency_ms_sum, 5.0);
    }

    #[test]
    fn accum_record_error_counts_latency_only() {
        let mut a = UpstreamAccum::default();
        a.record_error(5000.0);
        assert_eq!(a.request_count, 1);
        assert_eq!(a.narinfo_hits, 0);
        assert_eq!(a.narinfo_misses, 0);
        assert_eq!(a.latency_ms_sum, 5000.0);
    }
}
