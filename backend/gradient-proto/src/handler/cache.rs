/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::by_ids;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gradient_db::CacheDb;
use gradient_storage::NarStore;
use gradient_types::ids::{CacheId, CachedPathId, ProjectId};
use gradient_types::*;
use gradient_wire::transport::{
    Transport, external_arity_ok, may_consult_upstream_caches, pull_transport,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use tokio::sync::Semaphore;
use tracing::warn;

use super::SessionHandles;
use super::store_paths::project_mapper;

fn stored_sizes(
    cached_paths: &[gradient_entity::cached_path::Model],
) -> HashMap<String, (Option<i64>, Option<i64>)> {
    cached_paths
        .iter()
        .filter(|cp| cp.is_fully_cached())
        .map(|cp| (cp.hash.clone(), (cp.file_size, cp.nar_size)))
        .collect()
}

async fn load_cached_path_rows(
    db: &CacheDb,
    hashes: &[&str],
) -> Result<Vec<gradient_entity::cached_path::Model>, DbErr> {
    ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(hashes.to_vec()))
        .all(db)
        .await
}

gradient_db::sql! {
    INSERT_PUSH_SIGNATURE_PLACEHOLDERS = r#"
                INSERT INTO cached_path_signature (id, cached_path, cache, created_at)
                SELECT uuidv7(), cp.id, c.cache_id, (now() AT TIME ZONE 'UTC')
                FROM cached_path cp
                CROSS JOIN unnest($2::uuid[]) AS c(cache_id)
                WHERE cp.id = ANY($1::uuid[])
                ON CONFLICT (cached_path, cache) DO NOTHING
                "#,
        params = [
            Text("{11111111-1111-1111-1111-111111111111}"),
            Text("{22222222-2222-2222-2222-222222222222}"),
        ];
}

async fn ensure_push_signatures(
    db: &CacheDb,
    project_id: ProjectId,
    cached_path_rows: &[gradient_entity::cached_path::Model],
) {
    if cached_path_rows.is_empty() {
        return;
    }

    let project_caches = EProjectCache::find()
        .filter(CProjectCache::Project.eq(project_id))
        .all(db)
        .await
        .unwrap_or_default();
    if project_caches.is_empty() {
        return;
    }

    let cache_ids: Vec<uuid::Uuid> = project_caches
        .iter()
        .map(|oc| oc.cache.into_inner())
        .collect();
    let path_ids: Vec<uuid::Uuid> = cached_path_rows
        .iter()
        .map(|cp| cp.id.into_inner())
        .collect();

    const SIGNATURE_PATH_BATCH: usize = 8000;

    for chunk in path_ids.chunks(SIGNATURE_PATH_BATCH) {
        let result = db
            .execute_raw(
                INSERT_PUSH_SIGNATURE_PLACEHOLDERS
                    .bind([chunk.to_vec().into(), cache_ids.clone().into()]),
            )
            .await;
        if let Err(e) = result {
            warn!(
                %project_id,
                error = %e,
                "failed to insert cached_path_signature placeholders (Push mode)"
            );
        }
    }
}

/// A reply is carrying up to `CACHE_QUERY_MAX_PATHS` paths. Per-path lookups were putting about
/// 4000 sequential round trips in front of the 75 s deadline.
#[derive(Default)]
struct PullMetadata {
    references: HashMap<String, Vec<String>>,
    signatures: HashMap<CachedPathId, Vec<String>>,
}

impl PullMetadata {
    async fn load(
        db: &CacheDb,
        config: &RuntimeConfig,
        rows: &[gradient_entity::cached_path::Model],
    ) -> Result<Self, DbErr> {
        if rows.is_empty() {
            return Ok(Self::default());
        }

        let hashes: Vec<String> = rows.iter().map(|r| r.hash.clone()).collect();
        let references = gradient_db::graph::runtime_closure::references_for_hashes(db, &hashes)
            .await
            .unwrap_or_default();

        let ids: Vec<CachedPathId> = rows.iter().map(|r| r.id).collect();
        let signature_rows = gradient_db::fetch_in_chunks(&ids, |chunk| async move {
            ECachedPathSignature::find()
                .filter(CCachedPathSignature::CachedPath.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .unwrap_or_default();

        let cache_ids: Vec<CacheId> = signature_rows.iter().map(|r| r.cache).collect();
        let cache_names: HashMap<CacheId, String> = by_ids::<ECache, _>(db, &cache_ids)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(id, cache)| (id, cache.name))
            .collect();

        let serve_url = &config.server.serve_url;
        let mut signatures: HashMap<CachedPathId, Vec<String>> = HashMap::new();
        for row in signature_rows {
            let (Some(stored), Some(cache_name)) = (row.signature, cache_names.get(&row.cache))
            else {
                continue;
            };
            signatures.entry(row.cached_path).or_default().push(
                gradient_sources::full_signature_token(&stored, serve_url, cache_name),
            );
        }

        Ok(Self {
            references,
            signatures,
        })
    }

    fn references(&self, hash: &str) -> Option<Vec<String>> {
        let tokens = self.references.get(hash)?;
        expand_references(Some(&tokens.join(" ")))
    }

    fn signatures(&self, id: CachedPathId) -> Option<Vec<String>> {
        self.signatures.get(&id).cloned()
    }
}

#[derive(Default)]
struct PullFields {
    nar_hash: Option<String>,
    file_hash: Option<String>,
    references: Option<Vec<String>>,
    signatures: Option<Vec<String>>,
    deriver: Option<String>,
    ca: Option<String>,
}

fn pull_fields(
    row: Option<&gradient_entity::cached_path::Model>,
    meta: &PullMetadata,
) -> PullFields {
    let Some(row) = row else {
        return PullFields::default();
    };

    PullFields {
        nar_hash: row.nar_hash.clone(),
        file_hash: row.file_hash.clone(),
        references: meta.references(&row.hash),
        signatures: meta.signatures(row.id),
        deriver: row.deriver.clone(),
        ca: row.ca.clone(),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one wire entry; splitting it would only move the arguments"
)]
async fn build_cached_entry(
    config: &RuntimeConfig,
    nar_storage: &NarStore,
    hash: &str,
    path: &str,
    file_size: &Option<i64>,
    nar_size: &Option<i64>,
    mode: gradient_wire::types::QueryMode,
    expire: std::time::Duration,
    row: Option<&gradient_entity::cached_path::Model>,
    meta: &PullMetadata,
) -> gradient_wire::types::CachedPath {
    use gradient_wire::types::{CachedPath, QueryMode};

    if matches!(mode, QueryMode::Push) {
        return CachedPath {
            path: path.to_string(),
            cached: true,
            file_size: None,
            nar_size: None,
            url: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
    }

    let (url, fields) = match mode {
        QueryMode::Pull => {
            let confirmed = row.is_none_or(|r| r.confirmed);
            let size = file_size.unwrap_or(0).max(0) as u64;
            let transport = pull_transport(
                confirmed,
                size,
                config.nar.small_bytes,
                nar_storage.presigner_available(),
            );
            let url = match transport {
                Transport::Passthrough => None,
                Transport::Presigned => match nar_storage.presigned_get_url(hash, expire).await {
                    Ok(u) => u,
                    Err(e) => {
                        warn!(%hash, error = %e, "failed to generate presigned GET URL");
                        None
                    }
                },
            };
            (url, pull_fields(row, meta))
        }
        _ => (None, PullFields::default()),
    };

    CachedPath {
        path: path.to_string(),
        cached: true,
        file_size: file_size.map(|v| v as u64),
        nar_size: nar_size.map(|v| v as u64),
        url,
        nar_hash: fields.nar_hash,
        file_hash: fields.file_hash,
        references: fields.references,
        signatures: fields.signatures,
        deriver: fields.deriver,
        ca: fields.ca,
    }
}

fn uncached_push_entry(path: &str) -> gradient_wire::types::CachedPath {
    gradient_wire::types::CachedPath {
        path: path.to_string(),
        cached: false,
        file_size: None,
        nar_size: None,
        url: None,
        nar_hash: None,
        file_hash: None,
        references: None,
        signatures: None,
        deriver: None,
        ca: None,
    }
}

async fn extend_with_persisted_upstream(
    db: &CacheDb,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_wire::types::CachedPath>,
) -> HashSet<String> {
    let mut served = HashSet::new();
    if uncached_pairs.is_empty() {
        return served;
    }

    let hashes: Vec<String> = uncached_pairs.iter().map(|(h, _)| h.clone()).collect();
    let rows = match EDerivationOutput::find()
        .filter(CDerivationOutput::Hash.is_in(hashes))
        .filter(CDerivationOutput::ExternalUrl.is_not_null())
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "persisted upstream lookup failed");
            return served;
        }
    };

    let by_hash: HashMap<String, gradient_entity::derivation_output::Model> =
        rows.into_iter().map(|r| (r.hash.clone(), r)).collect();

    for (hash, path) in uncached_pairs {
        let Some(row) = by_hash.get(hash) else {
            continue;
        };
        let Some(url) = row.external_url.clone() else {
            continue;
        };
        result.push(gradient_wire::types::CachedPath {
            path: path.clone(),
            cached: true,
            file_size: row.file_size.map(|v| v as u64),
            nar_size: row.nar_size.map(|v| v as u64),
            url: Some(url),
            nar_hash: row.nar_hash.clone(),
            file_hash: row.file_hash.clone(),
            references: expand_references(row.references.as_deref()),
            signatures: None,
            deriver: row.deriver.clone(),
            ca: row.ca.clone(),
        });
        served.insert(hash.clone());
    }

    served
}

async fn extend_with_upstream_results(
    db: &CacheDb,
    upstream_query: &Arc<Semaphore>,
    project_id: ProjectId,
    mapper: &StorePathMapper,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_wire::types::CachedPath>,
) {
    const UPSTREAM_WINDOW_MINUTES: i64 = 60;

    let endpoints = match gradient_db::caches::upstream::upstream_endpoints_for_project(
        db,
        project_id,
        UPSTREAM_WINDOW_MINUTES,
    )
    .await
    {
        Ok(eps) => eps,
        Err(e) => {
            warn!(%project_id, error = %e, "CacheQuery upstream lookup failed");
            return;
        }
    };
    if endpoints.is_empty() {
        return;
    }

    let id_to_url: HashMap<_, String> = endpoints.iter().map(|e| (e.id, e.url.clone())).collect();

    let requested: HashMap<&str, &str> = uncached_pairs
        .iter()
        .map(|(hash, path)| (hash.as_str(), path.as_str()))
        .collect();
    let targets = uncached_pairs
        .iter()
        .filter_map(|(_, path)| StorePath::parse_stored(path).ok())
        .collect();
    let batch = gradient_core::upstream::probe_batch(
        endpoints,
        Arc::clone(upstream_query),
        mapper.clone(),
        targets,
    )
    .await;

    result.extend(batch.found.into_iter().map(|(hash, cp)| {
        gradient_wire::types::CachedPath {
            path: requested
                .get(hash.as_str())
                .map_or(cp.path.clone(), |p| (*p).to_owned()),
            ..cp
        }
    }));

    let mut by_url: HashMap<String, gradient_db::caches::upstream::UpstreamAccum> = HashMap::new();
    for (id, accum) in &batch.stats {
        if let Some(url) = id_to_url.get(id) {
            by_url.entry(url.clone()).or_default().merge(accum);
        }
    }

    let bucket = {
        use chrono::Timelike as _;
        let now = gradient_types::now();
        now.with_second(0)
            .and_then(|t: chrono::NaiveDateTime| t.with_nanosecond(0))
            .unwrap_or(now)
    };
    if let Err(e) =
        gradient_db::caches::upstream::upsert_upstream_metrics(db, bucket, &by_url).await
    {
        warn!(error = %e, "failed to flush upstream metrics");
    }
}

async fn extend_with_gradient_proto_results(
    db: &CacheDb,
    config: &RuntimeConfig,
    project_id: ProjectId,
    mapper: &StorePathMapper,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_wire::types::CachedPath>,
) {
    let upstream_caches =
        match gradient_db::caches::upstream::gradient_proto_upstream_caches_for_project(
            db, project_id,
        )
        .await
        {
            Ok(u) => u,
            Err(e) => {
                warn!(%project_id, error = %e, "gradient_proto upstream lookup failed");
                return;
            }
        };
    if upstream_caches.is_empty() {
        return;
    }

    let already: HashSet<String> = result.iter().map(|c| c.path.clone()).collect();
    let want: Vec<StorePath> = uncached_pairs
        .iter()
        .filter(|(_, p)| !already.contains(p))
        .filter_map(|(_, p)| StorePath::parse_stored(p).ok())
        .collect();
    if want.is_empty() {
        return;
    }

    for up in upstream_caches {
        let api_key = up
            .api_key_enc
            .as_deref()
            .and_then(|enc| gradient_sources::decrypt_secret(&config.secrets.crypt_file, enc).ok());
        let found = super::cache_consumer::pull_paths(
            &up.url,
            &up.remote_cache,
            api_key.as_deref(),
            mapper,
            &want,
        )
        .await;
        for cp in found {
            if !result.iter().any(|r| r.path == cp.path) {
                result.push(cp);
            }
        }
    }
}

async fn query(
    handles: &SessionHandles,
    project_id: ProjectId,
    paths: &[String],
    nar_sizes: &[Option<u64>],
    mode: gradient_wire::types::QueryMode,
    external: bool,
) -> Result<Vec<gradient_wire::types::CachedPath>, DbErr> {
    use gradient_wire::types::QueryMode;

    if matches!(mode, QueryMode::Push) && nar_sizes.len() != paths.len() {
        return Err(DbErr::Custom(format!(
            "nar_sizes has {} entries for {} paths",
            nar_sizes.len(),
            paths.len()
        )));
    }

    if !external_arity_ok(external, paths.len()) {
        return Err(DbErr::Custom(
            "an external CacheQuery names exactly one path".to_owned(),
        ));
    }

    let parsed = parse_stored_paths(paths);
    let hash_path_pairs: Vec<(&str, &str)> = parsed
        .iter()
        .map(|(stored, path)| (stored.hash(), *path))
        .collect();

    if hash_path_pairs.is_empty() {
        return Ok(vec![]);
    }

    let hashes: Vec<&str> = hash_path_pairs.iter().map(|(h, _)| *h).collect();

    let cached_path_rows = load_cached_path_rows(&handles.cache_db, &hashes).await?;
    let cached_map = stored_sizes(&cached_path_rows);

    if matches!(mode, QueryMode::Push) {
        ensure_push_signatures(&handles.cache_db, project_id, &cached_path_rows).await;
    }

    let rows_by_hash: HashMap<&str, &gradient_entity::cached_path::Model> = cached_path_rows
        .iter()
        .map(|r| (r.hash.as_str(), r))
        .collect();
    let meta = match mode {
        QueryMode::Pull => {
            PullMetadata::load(&handles.cache_db, &handles.db.config, &cached_path_rows).await?
        }
        _ => PullMetadata::default(),
    };

    let expire = gradient_wire::messages::PRESIGN_TTL;
    let mut result: Vec<gradient_wire::types::CachedPath> = Vec::new();

    for (hash, path) in &hash_path_pairs {
        if let Some((file_size, nar_size)) = cached_map.get(*hash) {
            result.push(
                build_cached_entry(
                    &handles.db.config,
                    &handles.db.storage.nar_storage,
                    hash,
                    path,
                    file_size,
                    nar_size,
                    mode,
                    expire,
                    rows_by_hash.get(hash).copied(),
                    &meta,
                )
                .await,
            );
        } else if matches!(mode, QueryMode::Push) {
            result.push(uncached_push_entry(path));
        }
    }

    if !may_consult_upstream_caches(mode, external) {
        return Ok(result);
    }

    let locally_cached_hashes: std::collections::HashSet<&str> =
        cached_map.keys().map(|s| s.as_str()).collect();
    let uncached_pairs: Vec<(String, String)> = hash_path_pairs
        .iter()
        .filter(|(hash, _)| !locally_cached_hashes.contains(hash))
        .map(|(h, p)| (h.to_string(), p.to_string()))
        .collect();

    let resolved =
        extend_with_persisted_upstream(&handles.cache_db, &uncached_pairs, &mut result).await;
    let uncached_pairs: Vec<(String, String)> = uncached_pairs
        .into_iter()
        .filter(|(h, _)| !resolved.contains(h))
        .collect();

    if uncached_pairs.is_empty() {
        return Ok(result);
    }
    if let Some(mapper) = project_mapper(&handles.db, project_id).await {
        extend_with_upstream_results(
            &handles.cache_db,
            &handles.upstream_query,
            project_id,
            &mapper,
            &uncached_pairs,
            &mut result,
        )
        .await;
        extend_with_gradient_proto_results(
            &handles.cache_db,
            &handles.db.config,
            project_id,
            &mapper,
            &uncached_pairs,
            &mut result,
        )
        .await;
    }

    Ok(result)
}

async fn hashes_in_cache(db: &CacheDb, cache_id: CacheId, hashes: &[&str]) -> HashSet<String> {
    if hashes.is_empty() {
        return HashSet::new();
    }

    let cached_paths = match ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(hashes.to_vec()))
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "cache-scoped query cached_path lookup failed");
            return HashSet::new();
        }
    };

    let id_to_hash: HashMap<CachedPathId, String> = cached_paths
        .into_iter()
        .map(|cp| (cp.id, cp.hash))
        .collect();
    if id_to_hash.is_empty() {
        return HashSet::new();
    }

    let ids: Vec<CachedPathId> = id_to_hash.keys().copied().collect();
    let signatures = match ECachedPathSignature::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCachedPathSignature::Cache.eq(cache_id))
                .add(CCachedPathSignature::CachedPath.is_in(ids)),
        )
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "cache-scoped query signature lookup failed");
            return HashSet::new();
        }
    };

    signatures
        .into_iter()
        .filter_map(|s| id_to_hash.get(&s.cached_path).cloned())
        .collect()
}

pub(super) async fn query_for_cache(
    handles: &SessionHandles,
    cache_id: CacheId,
    paths: &[String],
    mode: gradient_wire::types::QueryMode,
) -> Vec<gradient_wire::types::CachedPath> {
    use gradient_wire::types::QueryMode;

    if matches!(mode, QueryMode::Push) {
        return vec![];
    }

    let parsed = parse_stored_paths(paths);
    let hash_path_pairs: Vec<(&str, &str)> = parsed
        .iter()
        .map(|(stored, path)| (stored.hash(), *path))
        .collect();

    if hash_path_pairs.is_empty() {
        return vec![];
    }

    let hashes: Vec<&str> = hash_path_pairs.iter().map(|(h, _)| *h).collect();

    let in_cache = hashes_in_cache(&handles.cache_db, cache_id, &hashes).await;
    let cached_path_rows = load_cached_path_rows(&handles.cache_db, &hashes)
        .await
        .unwrap_or_default();
    let cached_map = stored_sizes(&cached_path_rows);

    let rows_by_hash: HashMap<&str, &gradient_entity::cached_path::Model> = cached_path_rows
        .iter()
        .map(|r| (r.hash.as_str(), r))
        .collect();
    let meta = match mode {
        QueryMode::Pull => {
            PullMetadata::load(&handles.cache_db, &handles.db.config, &cached_path_rows)
                .await
                .unwrap_or_default()
        }
        _ => PullMetadata::default(),
    };

    let expire = gradient_wire::messages::PRESIGN_TTL;
    let mut result: Vec<gradient_wire::types::CachedPath> = Vec::new();

    for (hash, path) in &hash_path_pairs {
        if !in_cache.contains(*hash) {
            continue;
        }
        if let Some((file_size, nar_size)) = cached_map.get(*hash) {
            result.push(
                build_cached_entry(
                    &handles.db.config,
                    &handles.db.storage.nar_storage,
                    hash,
                    path,
                    file_size,
                    nar_size,
                    mode,
                    expire,
                    rows_by_hash.get(hash).copied(),
                    &meta,
                )
                .await,
            );
        }
    }

    result
}

pub(super) async fn path_in_cache(db: &CacheDb, cache_id: CacheId, path: &StorePath) -> bool {
    !hashes_in_cache(db, cache_id, &[path.hash()])
        .await
        .is_empty()
}

fn parse_stored_paths(paths: &[String]) -> Vec<(StorePath, &str)> {
    paths
        .iter()
        .filter_map(|path| Some((StorePath::parse_stored(path).ok()?, path.as_str())))
        .collect()
}

pub(super) async fn handle_cache_query(
    handles: &SessionHandles,
    project_id: ProjectId,
    paths: &[String],
    nar_sizes: &[Option<u64>],
    mode: gradient_wire::types::QueryMode,
    external: bool,
) -> Result<Vec<gradient_wire::types::CachedPath>, DbErr> {
    query(handles, project_id, paths, nar_sizes, mode, external).await
}

fn expand_references(raw: Option<&str>) -> Option<Vec<String>> {
    raw.map(|s| {
        s.split_whitespace()
            .map(|r| {
                if r.starts_with("/nix/store/") {
                    r.to_owned()
                } else {
                    format!("/nix/store/{}", r)
                }
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::ServerState;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;
    use gradient_wire::types::QueryMode;
    use std::sync::Arc;

    fn cache_state_builder() -> TestState {
        let db = mock_db()
            .append_query_results(vec![
                Vec::<gradient_entity::derivation_output::Model>::new();
                8
            ])
            .into_connection();
        TestState::new().cache_db(db)
    }

    fn make_state() -> Arc<ServerState> {
        cache_state_builder().build()
    }

    #[tokio::test]
    async fn a_push_query_with_mismatched_sizes_is_an_error() {
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        assert!(
            query(
                &SessionHandles::from_state(&state),
                ProjectId::nil(),
                &paths,
                &[],
                QueryMode::Push,
                false
            )
            .await
            .is_err(),
            "a Push query must carry one size per path"
        );
    }

    fn make_s3_state() -> Arc<ServerState> {
        let nar_storage = gradient_storage::NarStore::s3(
            "bucket",
            "us-east-1",
            Some("http://127.0.0.1:1"),
            Some("key"),
            Some("secret"),
            "",
            false,
            gradient_storage::S3Timeouts::default(),
        )
        .expect("an S3 client builds and presigns without a network");
        cache_state_builder().nar_storage(nar_storage).build()
    }

    #[tokio::test]
    async fn push_answers_uncached_paths_without_any_grant() {
        let state = make_s3_state();
        let paths = vec![
            format!("/nix/store/{}-small", "a".repeat(32)),
            format!("/nix/store/{}-large", "b".repeat(32)),
        ];
        let sizes = [Some(1024), Some(8 * 1024 * 1024 * 1024)];
        let result = query(
            &SessionHandles::from_state(&state),
            ProjectId::nil(),
            &paths,
            &sizes,
            QueryMode::Push,
            false,
        )
        .await
        .unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|c| !c.cached && c.url.is_none()));
    }

    #[tokio::test]
    async fn cache_query_propagates_db_error_as_err() {
        use sea_orm::{DbErr, RuntimeErr};
        let db = mock_db()
            .append_query_errors(vec![DbErr::Conn(RuntimeErr::Internal(
                "Connection pool timed out".to_string(),
            ))])
            .into_connection();
        let state = TestState::new().cache_db(db).build();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        assert!(
            query(
                &SessionHandles::from_state(&state),
                ProjectId::nil(),
                &paths,
                &[],
                QueryMode::Pull,
                false
            )
            .await
            .is_err(),
            "DB error must propagate as Err, not a confident uncached result"
        );
    }

    #[tokio::test]
    async fn the_handler_refuses_a_two_path_external_query() {
        let state = make_state();
        let err = handle_cache_query(
            &SessionHandles::from_state(&state),
            ProjectId::nil(),
            &["/nix/store/a-a".to_owned(), "/nix/store/b-b".to_owned()],
            &[],
            QueryMode::Pull,
            true,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("exactly one path"), "{err}");
    }

    #[tokio::test]
    async fn cache_query_empty_paths_returns_empty() {
        let state = make_state();

        assert!(
            query(
                &SessionHandles::from_state(&state),
                ProjectId::nil(),
                &[],
                &[],
                QueryMode::Normal,
                false
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert!(
            query(
                &SessionHandles::from_state(&state),
                ProjectId::nil(),
                &[],
                &[],
                QueryMode::Push,
                false
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert!(
            query(
                &SessionHandles::from_state(&state),
                ProjectId::nil(),
                &[],
                &[],
                QueryMode::Pull,
                false
            )
            .await
            .unwrap()
            .is_empty()
        );
    }

    #[tokio::test]
    async fn cache_query_normal_uncached_returns_empty() {
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello".to_string()];
        let result = query(
            &SessionHandles::from_state(&state),
            ProjectId::nil(),
            &paths,
            &[],
            QueryMode::Normal,
            false,
        )
        .await
        .unwrap();
        assert!(
            result.is_empty(),
            "Normal mode should not return uncached paths"
        );
    }

    #[tokio::test]
    async fn cache_query_pull_returns_only_what_it_can_serve() {
        let state = make_state();
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar".to_string(),
        ];

        let result = query(
            &SessionHandles::from_state(&state),
            ProjectId::nil(),
            &paths,
            &[],
            QueryMode::Pull,
            false,
        )
        .await
        .unwrap();

        assert!(
            result.is_empty(),
            "an unserveable path is an omission, not an empty entry: {result:?}"
        );
    }

    #[tokio::test]
    async fn cache_query_push_uncached_returns_all_with_cached_false() {
        let state = make_state();
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar".to_string(),
        ];
        let result = query(
            &SessionHandles::from_state(&state),
            ProjectId::nil(),
            &paths,
            &[None; 2],
            QueryMode::Push,
            false,
        )
        .await
        .unwrap();
        assert_eq!(result.len(), 2, "Push should return all queried paths");
        for cp in &result {
            assert!(!cp.cached, "all should be uncached (empty DB): {}", cp.path);
            assert!(cp.url.is_none(), "local store -> no presigned URL");
            assert!(cp.file_size.is_none(), "Push carries no file_size");
            assert!(cp.nar_size.is_none(), "Push carries no nar_size");
            assert!(cp.nar_hash.is_none(), "Push carries no nar_hash");
            assert!(cp.references.is_none(), "Push carries no references");
            assert!(cp.signatures.is_none(), "Push carries no signatures");
            assert!(cp.deriver.is_none(), "Push carries no deriver");
            assert!(cp.ca.is_none(), "Push carries no ca");
        }
        let returned_paths: Vec<&str> = result.iter().map(|c| c.path.as_str()).collect();
        assert!(returned_paths.contains(&paths[0].as_str()));
        assert!(returned_paths.contains(&paths[1].as_str()));
    }

    #[tokio::test]
    async fn cache_scoped_query_rejects_push() {
        let state = make_state();
        let cache = CacheId::new(uuid::Uuid::now_v7());
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        assert!(
            query_for_cache(
                &SessionHandles::from_state(&state),
                cache,
                &paths,
                QueryMode::Push
            )
            .await
            .is_empty()
        );
    }

    #[tokio::test]
    async fn cache_scoped_query_empty_returns_empty() {
        let state = make_state();
        let cache = CacheId::new(uuid::Uuid::now_v7());
        assert!(
            query_for_cache(
                &SessionHandles::from_state(&state),
                cache,
                &[],
                QueryMode::Normal
            )
            .await
            .is_empty()
        );
    }

    #[test]
    fn expand_references_none_passthrough() {
        assert_eq!(expand_references(None), None);
    }

    #[test]
    fn expand_references_empty_string_empty_vec() {
        assert_eq!(expand_references(Some("")), Some(vec![]));
    }

    #[test]
    fn expand_references_mixed_forms() {
        let out = expand_references(Some("aaaa-a /nix/store/bbbb-b")).unwrap();
        assert_eq!(
            out,
            vec![
                "/nix/store/aaaa-a".to_string(),
                "/nix/store/bbbb-b".to_string(),
            ]
        );
    }

    #[test]
    fn expand_references_collapses_multiple_whitespace() {
        let out = expand_references(Some("aaaa-a   \t bbbb-b")).unwrap();
        assert_eq!(out.len(), 2);
    }
}
