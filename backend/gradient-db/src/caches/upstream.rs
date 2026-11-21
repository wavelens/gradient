/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_entity::cache::{Entity as ECache, Model as MCache};
use gradient_entity::cache_upstream::{
    CacheUpstreamKind, Column as CCacheUpstream, Entity as ECacheUpstream, Model as MCacheUpstream,
};
use gradient_entity::derivation_output::{
    Column as CDerivationOutput, Entity as EDerivationOutput, Model as MDerivationOutput,
};
use gradient_entity::project_cache::{
    CacheSubscriptionMode, Column as CProjectCache, Entity as EProjectCache,
};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QuerySelect,
    TransactionTrait,
};

use gradient_types::ids::{CacheId, CacheUpstreamId, EvaluationId, ProjectId};
use gradient_util::http_base_url::{
    ValidatedHttpBaseUrl,
    upstream_cache_url_for_display_without_username_password_query_and_fragment,
};

pub fn parse_stored_upstream_cache_url(
    upstream: CacheUpstreamId,
    raw: &str,
) -> Option<ValidatedHttpBaseUrl> {
    raw.parse()
        .inspect_err(|error| {
            tracing::warn!(%upstream, %error, "Skip upstream cache with an invalid base URL.");
        })
        .ok()
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamEndpoint {
    pub id: CacheUpstreamId,
    pub url: ValidatedHttpBaseUrl,
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
    pub url: ValidatedHttpBaseUrl,
    pub remote_cache: String,
    pub public_key: Option<String>,
    pub encrypted_credential: Option<String>,
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
                url: parse_stored_upstream_cache_url(r.id, r.url.as_deref()?)?,
                remote_cache: r.remote_cache_name?,
                public_key: r.public_key,
                encrypted_credential: r.encrypted_credential,
            })
        })
        .collect())
}

/// Workers download NARs directly from upstream caches. Store no credential for such a cache.
const SQL_WHERE_FOR_UPSTREAM_CACHES_WORKERS_MAY_USE: &str = "cu.kind = 2 AND cu.mode <> 2 AND cu.active \
     AND cu.url IS NOT NULL AND cu.encrypted_credential IS NULL";

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
         WHERE oc.project = $1 AND oc.mode <> 2 AND {SQL_WHERE_FOR_UPSTREAM_CACHES_WORKERS_MAY_USE} \
         GROUP BY cu.id, cu.url, cu.public_key, cu.http1_only",
    )
}

pub async fn find_build_outputs_by_hashes_with_nar_urls_from_upstream_caches_workers_may_use<
    C: ConnectionTrait,
>(
    db: &C,
    hashes: Vec<String>,
) -> Result<Vec<MDerivationOutput>, DbErr> {
    EDerivationOutput::find()
        .filter(CDerivationOutput::Hash.is_in(hashes))
        .filter(CDerivationOutput::ExternalUrl.is_not_null())
        .filter(Expr::cust(format!(
            "\"derivation_output\".\"verified_by_upstream\" IN \
             (SELECT cu.id FROM cache_upstream cu WHERE {SQL_WHERE_FOR_UPSTREAM_CACHES_WORKERS_MAY_USE})"
        )))
        .all(db)
        .await
}

pub async fn clear_nar_url_and_upstream_reference_for_build_outputs_verified_by_upstream<
    C: ConnectionTrait,
>(
    db: &C,
    upstream: CacheUpstreamId,
) -> Result<(), DbErr> {
    EDerivationOutput::update_many()
        .col_expr(CDerivationOutput::ExternalUrl, Expr::value(None::<String>))
        .col_expr(
            CDerivationOutput::VerifiedByUpstream,
            Expr::value(None::<uuid::Uuid>),
        )
        .filter(CDerivationOutput::VerifiedByUpstream.eq(upstream))
        .exec(db)
        .await?;
    Ok(())
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
            let id = CacheUpstreamId::new(id);
            let url: String = r.try_get("", "url").ok()?;
            Some(UpstreamEndpoint {
                url: parse_stored_upstream_cache_url(id, &url)?,
                id,
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
        .filter_map(|r| r.try_get::<String>("", "url").ok())
        .filter_map(|stored| {
            Some(
                upstream_cache_url_for_display_without_username_password_query_and_fragment(
                    &stored,
                )?
                .url,
            )
        })
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
        .filter_map(|stored| {
            Some(
                upstream_cache_url_for_display_without_username_password_query_and_fragment(
                    &stored,
                )?
                .url,
            )
        })
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

pub const MAX_UPSTREAM_CACHE_COUNT: usize = 32;

/// Lock the cache row so parallel requests don't both pass the count check and go over the limit.
pub async fn insert_upstream_cache_if_below_max_upstream_count<
    C: TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
>(
    db: &C,
    upstream: MCacheUpstream,
) -> Result<Option<MCacheUpstream>, DbErr> {
    let tx = db.begin().await?;
    ECache::find_by_id(upstream.cache)
        .lock_exclusive()
        .one(&tx)
        .await?;
    let existing = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(upstream.cache))
        .count(&tx)
        .await?;
    if existing >= MAX_UPSTREAM_CACHE_COUNT as u64 {
        return Ok(None);
    }
    let inserted = crate::rows::insert::<ECacheUpstream, _>(&tx, upstream).await?;
    tx.commit().await?;
    Ok(Some(inserted))
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

    #[tokio::test]
    async fn test_worker_upstream_list_omits_credentialed_upstream() {
        use gradient_entity::cache_upstream::UpstreamCredentialScheme;
        use gradient_entity::ids::ProjectCacheId;
        use gradient_fixtures::rows;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        crate::rows::insert::<gradient_entity::user::Entity, _>(&connection, rows::user())
            .await
            .expect("insert user");
        crate::rows::insert::<gradient_entity::cache::Entity, _>(&connection, rows::cache())
            .await
            .expect("insert cache");
        let project = crate::rows::insert::<gradient_entity::project::Entity, _>(
            &connection,
            rows::project(),
        )
        .await
        .expect("insert project");
        crate::rows::insert::<EProjectCache, _>(
            &connection,
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                project.id,
                rows::cache().id,
                CacheSubscriptionMode::ReadOnly,
            ),
        )
        .await
        .expect("insert project cache");
        let open = crate::rows::insert::<ECacheUpstream, _>(
            &connection,
            rows::external_upstream(
                CacheUpstreamId::now_v7(),
                rows::cache().id,
                "https://open.example",
                "open.example-1:key",
            ),
        )
        .await
        .expect("insert upstream cache");
        crate::rows::insert::<ECacheUpstream, _>(
            &connection,
            MCacheUpstream {
                encrypted_credential: Some("c2VjcmV0LWNpcGhlcnRleHQ=".into()),
                credential_scheme: Some(UpstreamCredentialScheme::Basic),
                ..rows::external_upstream(
                    CacheUpstreamId::now_v7(),
                    rows::cache().id,
                    "https://private.example",
                    "private.example-1:key",
                )
            },
        )
        .await
        .expect("insert upstream cache");

        let endpoints = upstream_endpoints_for_project(&connection, project.id, 60)
            .await
            .expect("list upstream endpoints");

        let ids: Vec<CacheUpstreamId> = endpoints.iter().map(|e| e.id).collect();
        assert_eq!(ids, [open.id]);
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_clears_nar_urls_with_credentials_or_query_for_all_upstream_caches() {
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;
        use sea_orm_migration::{MigratorTrait, SchemaManager};

        let persisted = [
            (
                "a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2",
                "https://u:p@old.example/nar/a.nar",
            ),
            (
                "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2",
                "https://user@old.example/nar/b.nar",
            ),
            (
                "c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2",
                "https://old.example/?token=x/nar/c.nar",
            ),
            (
                "d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2",
                "https://open.example/nar/d@e.nar",
            ),
        ];
        let mut builder = shape().evaluation(EvaluationId::now_v7());
        for (hash, _) in persisted {
            builder = builder
                .derivation(&format!("{hash}-x.drv"))
                .cache_available()
                .output(hash);
        }
        let db = builder.into_database().await;
        let connection = db.connection();
        for (hash, url) in persisted {
            EDerivationOutput::update_many()
                .col_expr(CDerivationOutput::ExternalUrl, Expr::value(url))
                .filter(CDerivationOutput::Hash.eq(hash))
                .exec(&connection)
                .await
                .unwrap();
        }
        let migration = gradient_migration::Migrator::migrations()
            .into_iter()
            .find(|m| {
                m.name()
                    == "m20261226_000000_clear_persisted_upstream_nar_urls_with_user_password_or_query"
            })
            .expect("register the migration to remove secrets from persisted NAR URLs");

        migration
            .up(&SchemaManager::new(&connection))
            .await
            .expect("the migration runs");

        let mut remaining: Vec<_> = db
            .rows::<EDerivationOutput>()
            .await
            .into_iter()
            .filter_map(|o| o.external_url)
            .collect();
        remaining.sort();
        assert_eq!(remaining, ["https://open.example/nar/d@e.nar"]);
        db.finish().await;
    }

    #[tokio::test]
    async fn test_workers_get_persisted_nar_urls_only_from_active_external_credential_free_upstream_caches()
     {
        use gradient_entity::cache_upstream::{
            ActiveModel as ACacheUpstream, UpstreamCredentialScheme,
        };
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;
        use sea_orm::ActiveValue::Set;

        let upstreams = [
            (
                "a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3",
                CacheUpstreamId::now_v7(),
                true,
            ),
            (
                "b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3",
                CacheUpstreamId::now_v7(),
                true,
            ),
            (
                "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3",
                CacheUpstreamId::now_v7(),
                false,
            ),
        ];
        let [open, private, _] = upstreams.map(|(_, id, _)| id);
        let mut builder = shape().evaluation(EvaluationId::now_v7());
        for (hash, upstream, _) in upstreams {
            builder = builder
                .derivation(&format!("{hash}-x.drv"))
                .cache_available()
                .output(hash)
                .verified_by(upstream);
        }
        builder = builder
            .derivation("hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh-orphan.drv")
            .cache_available()
            .output("d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3")
            .into_cache(CacheId::now_v7());
        for (hash, upstream, active) in upstreams {
            builder = builder.upstream(
                upstream,
                &format!("https://{hash}.example"),
                "upstream.example-1:key",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadOnly,
                active,
            );
        }
        let db = builder.into_database().await;
        let connection = db.connection();
        crate::rows::update::<ECacheUpstream, _>(
            &connection,
            ACacheUpstream {
                id: Set(private),
                encrypted_credential: Set(Some("c2VjcmV0LWNpcGhlcnRleHQ=".into())),
                credential_scheme: Set(Some(UpstreamCredentialScheme::Basic)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let all_hashes: Vec<String> = upstreams
            .iter()
            .map(|(hash, _, _)| hash.to_string())
            .chain(["d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3".to_owned()])
            .collect();

        let offered =
            find_build_outputs_by_hashes_with_nar_urls_from_upstream_caches_workers_may_use(
                &connection,
                all_hashes,
            )
            .await
            .unwrap();

        let offered: Vec<_> = offered
            .into_iter()
            .map(|o| o.verified_by_upstream)
            .collect();
        assert_eq!(offered, [Some(open)]);
        db.finish().await;
    }

    #[tokio::test]
    async fn deleting_upstream_cache_clears_persisted_nar_urls() {
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;

        const BUILD_OUTPUT_HASH_WHOSE_UPSTREAM_CACHE_IS_DELETED_BY_SINGLE_DELETE_PATH: &str =
            "a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4";
        const BUILD_OUTPUT_HASH_OF_REMOVED_UPSTREAM_CACHE: &str =
            "b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4";
        let (one_by_one, by_state_apply) = (CacheUpstreamId::now_v7(), CacheUpstreamId::now_v7());
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation("iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-a.drv")
            .cache_available()
            .output(BUILD_OUTPUT_HASH_WHOSE_UPSTREAM_CACHE_IS_DELETED_BY_SINGLE_DELETE_PATH)
            .verified_by(one_by_one)
            .derivation("mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm-b.drv")
            .cache_available()
            .output(BUILD_OUTPUT_HASH_OF_REMOVED_UPSTREAM_CACHE)
            .verified_by(by_state_apply)
            .into_cache(cache)
            .upstream(
                one_by_one,
                "https://one.example",
                "one.example-1:key",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadOnly,
                true,
            )
            .upstream(
                by_state_apply,
                "https://state.example",
                "state.example-1:key",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadOnly,
                true,
            )
            .into_database()
            .await;
        let connection = db.connection();

        ECacheUpstream::delete_by_id(one_by_one)
            .exec(&connection)
            .await
            .unwrap();
        ECacheUpstream::delete_many()
            .filter(CCacheUpstream::Cache.eq(cache))
            .exec(&connection)
            .await
            .unwrap();

        let outputs = db.rows::<EDerivationOutput>().await;
        let persisted: Vec<_> = outputs
            .iter()
            .map(|o| (o.external_url.clone(), o.verified_by_upstream))
            .collect();
        assert_eq!(persisted, [(None, None), (None, None)]);
        let counted_as_available =
            crate::derivation_outputs::output_ids_skipping_build_or_probe_for_external_url_or_cached_variant_trusted_by_shared_build(&connection, &outputs)
                .await
                .unwrap();
        assert!(counted_as_available.is_empty(), "{counted_as_available:?}");
        let offered =
            find_build_outputs_by_hashes_with_nar_urls_from_upstream_caches_workers_may_use(
                &connection,
                [
                    BUILD_OUTPUT_HASH_WHOSE_UPSTREAM_CACHE_IS_DELETED_BY_SINGLE_DELETE_PATH,
                    BUILD_OUTPUT_HASH_OF_REMOVED_UPSTREAM_CACHE,
                ]
                .map(str::to_owned)
                .to_vec(),
            )
            .await
            .unwrap();
        assert!(offered.is_empty(), "{offered:?}");
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_normalises_stored_upstream_cache_urls_of_both_kinds() {
        use sea_orm_migration::{MigratorTrait, SchemaManager};

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let migration = gradient_migration::Migrator::migrations()
            .into_iter()
            .find(|m| m.name() == "m20261224_000000_normalise_upstream_cache_urls")
            .expect("register the migration that normalises upstream cache URLs");
        crate::rows::insert::<gradient_entity::user::Entity, _>(
            &connection,
            gradient_fixtures::rows::user(),
        )
        .await
        .expect("insert user");
        crate::rows::insert::<gradient_entity::cache::Entity, _>(
            &connection,
            gradient_fixtures::rows::cache(),
        )
        .await
        .expect("insert cache");
        let stored = [
            (CacheUpstreamKind::Http, "https://Cache.Example.com/"),
            (CacheUpstreamKind::Http, "https://cache.nixos.org"),
            (CacheUpstreamKind::Http, "https://query.example/?a=1"),
            (
                CacheUpstreamKind::GradientProto,
                "http://remote.example:80/prefix/",
            ),
            (CacheUpstreamKind::GradientProto, "https://remote.example"),
        ];
        let mut ids = Vec::new();
        for (kind, url) in stored {
            let row = crate::rows::insert::<ECacheUpstream, _>(
                &connection,
                MCacheUpstream {
                    id: CacheUpstreamId::now_v7(),
                    cache: gradient_fixtures::rows::cache().id,
                    display_name: url.into(),
                    kind,
                    url: Some(url.into()),
                    ..Default::default()
                },
            )
            .await
            .expect("insert upstream cache");
            ids.push(row.id);
        }

        migration
            .up(&SchemaManager::new(&connection))
            .await
            .expect("the migration runs");

        let mut normalised = Vec::new();
        for id in ids {
            let row = db.find::<ECacheUpstream>(id).await.expect("row remains");
            normalised.push(row.url.expect("url remains"));
        }
        assert_eq!(
            normalised,
            [
                "https://cache.example.com",
                "https://cache.nixos.org",
                "https://query.example/?a=1",
                "http://remote.example/prefix",
                "https://remote.example",
            ]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_sets_encrypted_credential_from_old_api_key_with_bearer_scheme_on_insert_and_update_and_revert_keeps_api_key()
     {
        use gradient_entity::cache_upstream::UpstreamCredentialScheme;
        use sea_orm_migration::{MigratorTrait, SchemaManager};

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let migration = gradient_migration::Migrator::migrations()
            .into_iter()
            .find(|m| {
                m.name()
                    == "m20261225_000000_add_credential_columns_to_cache_upstream_and_copy_api_key"
            })
            .expect("Register a migration to add an encrypted credentials column to the upstream cache.");
        crate::rows::insert::<gradient_entity::user::Entity, _>(
            &connection,
            gradient_fixtures::rows::user(),
        )
        .await
        .expect("insert user");
        crate::rows::insert::<gradient_entity::cache::Entity, _>(
            &connection,
            gradient_fixtures::rows::cache(),
        )
        .await
        .expect("insert cache");
        let stored = [
            (
                CacheUpstreamKind::GradientProto,
                "https://remote.example",
                Some("c2VjcmV0LWNpcGhlcnRleHQ="),
            ),
            (
                CacheUpstreamKind::GradientProto,
                "https://open.example",
                None,
            ),
            (CacheUpstreamKind::Http, "https://cache.nixos.org", None),
        ];
        let mut ids = Vec::new();
        for (kind, url, api_key) in stored {
            let row = crate::rows::insert::<ECacheUpstream, _>(
                &connection,
                MCacheUpstream {
                    id: CacheUpstreamId::now_v7(),
                    cache: gradient_fixtures::rows::cache().id,
                    display_name: url.into(),
                    kind,
                    url: Some(url.into()),
                    encrypted_credential: api_key.map(str::to_owned),
                    credential_scheme: api_key.map(|_| UpstreamCredentialScheme::Bearer),
                    ..Default::default()
                },
            )
            .await
            .expect("insert upstream cache");
            ids.push(row.id);
        }

        let schema = SchemaManager::new(&connection);
        migration.down(&schema).await.expect("revert the migration");
        migration.up(&schema).await.expect("the migration runs");

        let mut credentials = Vec::new();
        for id in ids {
            let row = db.find::<ECacheUpstream>(id).await.expect("row remains");
            credentials.push((row.encrypted_credential, row.credential_scheme));
        }
        assert_eq!(
            credentials,
            [
                (
                    Some("c2VjcmV0LWNpcGhlcnRleHQ=".to_owned()),
                    Some(UpstreamCredentialScheme::Bearer),
                ),
                (None, None),
                (None, None),
            ]
        );

        db.execute(
            "INSERT INTO public.cache_upstream \
             (id, cache, display_name, mode, kind, url, api_key, http1_only, active) \
             SELECT gen_random_uuid(), cache, 'https://old.example', mode, kind, \
             'https://old.example', 'b2xkLWluc2VydA==', http1_only, active \
             FROM public.cache_upstream WHERE url = 'https://open.example'",
        )
        .await;
        db.execute(
            "UPDATE public.cache_upstream SET api_key = 'b2xkLXVwZGF0ZQ==' \
             WHERE url = 'https://open.example'",
        )
        .await;
        let credential_query = "SELECT url || ' ' || coalesce(encrypted_credential, '-') || ' ' \
             || coalesce(credential_scheme::text, '-') FROM public.cache_upstream \
             WHERE url IN ('https://old.example', 'https://open.example') ORDER BY url";
        assert_eq!(
            db.texts(credential_query).await,
            [
                "https://old.example b2xkLWluc2VydA== 0",
                "https://open.example b2xkLXVwZGF0ZQ== 0",
            ],
            "a key that a previous release binary inserted or updated in the old api_key column can reach encrypted_credential, so the new binary does not query a private upstream without a key"
        );

        migration.down(&schema).await.expect("revert the migration");
        assert_eq!(
            db.texts(
                "SELECT url || ' ' || coalesce(api_key, '-') FROM public.cache_upstream ORDER BY url"
            )
            .await,
            [
                "https://cache.nixos.org -",
                "https://old.example b2xkLWluc2VydA==",
                "https://open.example b2xkLXVwZGF0ZQ==",
                "https://remote.example c2VjcmV0LWNpcGhlcnRleHQ=",
            ],
            "rolled-back migration kept all bearer keys in api_key"
        );
        migration.up(&schema).await.expect("the migration runs");
        db.finish().await;
    }
}
