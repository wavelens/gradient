/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::by_ids;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gradient_core::{WorkerJob, WorkerJobPolicy};
use gradient_db::CacheDb;
use gradient_db::store_path_content_variant_reader_choice::StorePathContentVariantReader;
use gradient_storage::NarStore;
use gradient_types::ids::{CacheId, CacheUpstreamId, CachedPathId, ProjectId};
use gradient_types::*;
use gradient_wire::transport::{
    Transport, external_arity_ok, may_consult_upstream_caches, pull_transport,
};
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};
use tracing::warn;

use super::SessionHandles;
use super::store_paths::project_mapper;

type ContentVariantByStoreHash = HashMap<String, gradient_entity::cached_path::Model>;

#[derive(Debug, Clone)]
pub(super) struct CacheQueryJob {
    pub(super) project: ProjectId,
    pub(super) content_variant_reader: StorePathContentVariantReader,
}

impl CacheQueryJob {
    #[cfg(test)]
    pub(super) fn for_test_project(project: ProjectId) -> Self {
        Self {
            project,
            content_variant_reader: StorePathContentVariantReader::Project(project),
        }
    }
}

async fn ensure_push_signatures(
    db: &CacheDb,
    project_id: ProjectId,
    trusted: &ContentVariantByStoreHash,
) {
    let hashes: Vec<String> = trusted.keys().cloned().collect();

    const SIGNATURE_PATH_BATCH: usize = 8000;

    for chunk in hashes.chunks(SIGNATURE_PATH_BATCH) {
        let result = gradient_db::store_path_content_variant_trust::insert_unsigned_signature_placeholders_for_trusted_content_variants(
            db, project_id, chunk,
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
    signatures: HashMap<CachedPathId, Vec<String>>,
}

impl PullMetadata {
    async fn load(
        db: &CacheDb,
        config: &RuntimeConfig,
        rows: &ContentVariantByStoreHash,
        readable_binary_cache_ids: &[CacheId],
    ) -> Result<Self, DbErr> {
        if rows.is_empty() || readable_binary_cache_ids.is_empty() {
            return Ok(Self::default());
        }

        let ids: Vec<CachedPathId> = rows.values().map(|r| r.id).collect();
        let signature_rows = gradient_db::fetch_in_chunks(&ids, |chunk| async move {
            ECachedPathSignature::find()
                .filter(CCachedPathSignature::CachedPath.is_in(chunk))
                .filter(CCachedPathSignature::Cache.is_in(readable_binary_cache_ids.to_vec()))
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

        Ok(Self { signatures })
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

fn pull_fields(row: &gradient_entity::cached_path::Model, meta: &PullMetadata) -> PullFields {
    PullFields {
        nar_hash: Some(row.nar_hash.clone()),
        file_hash: Some(row.file_hash.clone()),
        references: expand_references(row.references.as_deref()),
        signatures: meta.signatures(row.id),
        deriver: row.deriver.clone(),
        ca: row.ca.clone(),
    }
}

async fn build_cached_entry(
    config: &RuntimeConfig,
    nar_storage: &NarStore,
    path: &str,
    mode: gradient_wire::types::QueryMode,
    expire: std::time::Duration,
    row: &gradient_entity::cached_path::Model,
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
            let size = row.file_size.unwrap_or(0).max(0) as u64;
            let transport = pull_transport(
                size,
                config.nar.small_bytes,
                nar_storage.presigner_available(),
            );
            let key = &row.nar_object_storage_key;
            let url = match transport {
                Transport::Passthrough => None,
                Transport::Presigned => match nar_storage.presigned_get_url(key, expire).await {
                    Ok(u) => u,
                    Err(e) => {
                        warn!(%key, error = %e, "failed to generate presigned GET URL");
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
        file_size: row.file_size.map(|v| v as u64),
        nar_size: Some(row.nar_size as u64),
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

#[derive(Clone, Copy)]
struct AllowedUpstreams<'a> {
    worker: &'a str,
    policy: &'a dyn WorkerJobPolicy,
}

impl AllowedUpstreams<'_> {
    fn allow(&self, public_key: Option<&str>) -> bool {
        self.policy.allows(
            self.worker,
            WorkerJob::Substitution {
                verifying_keys: &[public_key.map(str::to_owned)],
            },
        )
    }
}

async fn extend_with_persisted_upstream(
    db: &CacheDb,
    allowed: AllowedUpstreams<'_>,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_wire::types::CachedPath>,
) -> HashSet<String> {
    let mut served = HashSet::new();
    if uncached_pairs.is_empty() {
        return served;
    }

    let hashes: Vec<String> = uncached_pairs.iter().map(|(h, _)| h.clone()).collect();
    let rows =
        match gradient_db::caches::upstream::find_build_outputs_by_hashes_with_nar_urls_from_upstream_caches_workers_may_use(
            db, hashes,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "persisted upstream lookup failed");
                return served;
            }
        };

    let upstream_ids: Vec<CacheUpstreamId> =
        rows.iter().filter_map(|r| r.verified_by_upstream).collect();
    let upstreams = match by_ids::<ECacheUpstream, _>(db, &upstream_ids).await {
        Ok(upstreams) => upstreams,
        Err(e) => {
            warn!(error = %e, "Lookup of persisted upstream key failed");
            return served;
        }
    };
    let by_hash: HashMap<String, gradient_entity::derivation_output::Model> = rows
        .into_iter()
        .filter(|r| {
            let key = r
                .verified_by_upstream
                .and_then(|id| upstreams.get(&id))
                .and_then(|u| u.public_key.as_deref());
            allowed.allow(key)
        })
        .filter_map(|r| Some((r.hash.clone()?, r)))
        .collect();

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
            url: Some(url.into()),
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
    handles: &SessionHandles,
    allowed: AllowedUpstreams<'_>,
    project_id: ProjectId,
    mapper: &StorePathMapper,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_wire::types::CachedPath>,
) {
    const UPSTREAM_WINDOW_MINUTES: i64 = 60;
    let db = &handles.cache_db;

    let endpoints = match gradient_db::caches::upstream::upstream_endpoints_for_project(
        db,
        project_id,
        UPSTREAM_WINDOW_MINUTES,
    )
    .await
    {
        Ok(eps) => eps
            .into_iter()
            .filter(|e| allowed.allow(e.public_key.as_deref()))
            .collect::<Vec<_>>(),
        Err(e) => {
            warn!(%project_id, error = %e, "CacheQuery upstream lookup failed");
            return;
        }
    };
    if endpoints.is_empty() {
        return;
    }

    let id_to_url: HashMap<_, String> = endpoints
        .iter()
        .map(|e| (e.id, e.url.to_string()))
        .collect();

    let requested: HashMap<&str, &str> = uncached_pairs
        .iter()
        .map(|(hash, path)| (hash.as_str(), path.as_str()))
        .collect();
    let targets = uncached_pairs
        .iter()
        .filter_map(|(_, path)| StorePath::parse_stored(path).ok())
        .collect();
    let batch = gradient_core::upstream::probe_batch(
        &handles.db.user_supplied_url_http_client,
        endpoints,
        Arc::clone(&handles.upstream_query),
        mapper.clone(),
        targets,
    )
    .await;

    result.extend(batch.found.into_iter().map(|(hash, found)| {
        gradient_wire::types::CachedPath {
            path: requested
                .get(hash.as_str())
                .map_or(found.path.path.clone(), |p| (*p).to_owned()),
            ..found.path
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
    handles: &SessionHandles,
    project_id: ProjectId,
    mapper: &StorePathMapper,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_wire::types::CachedPath>,
) {
    let upstream_caches =
        match gradient_db::caches::upstream::gradient_proto_upstream_caches_for_project(
            &handles.cache_db,
            project_id,
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
        let api_key = up.encrypted_credential.as_deref().and_then(|enc| {
            gradient_sources::decrypt_secret(&handles.db.config.secrets.crypt_file, enc).ok()
        });
        let found = super::cache_consumer::pull_paths(
            &up.url,
            &up.remote_cache,
            api_key.as_ref().map(SecretString::expose),
            handles.db.user_supplied_url_http_client.allowlist(),
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
    worker: &str,
    job: CacheQueryJob,
    paths: &[String],
    nar_sizes: &[Option<u64>],
    mode: gradient_wire::types::QueryMode,
    external: bool,
) -> Result<Vec<gradient_wire::types::CachedPath>, DbErr> {
    use gradient_wire::types::QueryMode;

    let project_id = job.project;
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

    let hashes: Vec<String> = hash_path_pairs
        .iter()
        .map(|(h, _)| (*h).to_owned())
        .collect();

    let trusted = gradient_db::store_path_content_variant_reader_choice::get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
        &handles.cache_db,
        job.content_variant_reader,
        &hashes,
    )
    .await?;

    if matches!(mode, QueryMode::Push) {
        ensure_push_signatures(&handles.cache_db, project_id, &trusted).await;
    }

    let meta = match mode {
        QueryMode::Pull if !trusted.is_empty() => {
            let readable_binary_cache_ids =
                gradient_db::cache_paths::project_read_caches(&handles.cache_db, project_id)
                    .await?;
            PullMetadata::load(
                &handles.cache_db,
                &handles.db.config,
                &trusted,
                &readable_binary_cache_ids,
            )
            .await?
        }
        _ => PullMetadata::default(),
    };

    let expire = gradient_wire::messages::PRESIGN_TTL;
    let mut result: Vec<gradient_wire::types::CachedPath> = Vec::new();

    for (hash, path) in &hash_path_pairs {
        if let Some(row) = trusted.get(*hash) {
            result.push(
                build_cached_entry(
                    &handles.db.config,
                    &handles.db.storage.nar_storage,
                    path,
                    mode,
                    expire,
                    row,
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

    let uncached_pairs: Vec<(String, String)> = hash_path_pairs
        .iter()
        .filter(|(hash, _)| !trusted.contains_key(*hash))
        .map(|(h, p)| (h.to_string(), p.to_string()))
        .collect();

    let allowed = AllowedUpstreams {
        worker,
        policy: &*handles.worker_job_policy,
    };
    let resolved =
        extend_with_persisted_upstream(&handles.cache_db, allowed, &uncached_pairs, &mut result)
            .await;
    let uncached_pairs: Vec<(String, String)> = uncached_pairs
        .into_iter()
        .filter(|(h, _)| !resolved.contains(h))
        .collect();

    if uncached_pairs.is_empty() {
        return Ok(result);
    }
    if let Some(mapper) = project_mapper(&handles.db, project_id).await {
        extend_with_upstream_results(
            handles,
            allowed,
            project_id,
            &mapper,
            &uncached_pairs,
            &mut result,
        )
        .await;
        if allowed.allow(None) {
            extend_with_gradient_proto_results(
                handles,
                project_id,
                &mapper,
                &uncached_pairs,
                &mut result,
            )
            .await;
        }
    }

    Ok(result)
}

async fn content_variants_served_by_cache(
    db: &CacheDb,
    cache_id: CacheId,
    hashes: &[String],
) -> ContentVariantByStoreHash {
    gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
        db, cache_id, hashes,
    )
    .await
    .unwrap_or_else(|e| {
        warn!(%cache_id, error = %e, "cache-scoped query content variant lookup failed");
        HashMap::new()
    })
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

    let hashes: Vec<String> = hash_path_pairs
        .iter()
        .map(|(h, _)| (*h).to_owned())
        .collect();
    let served = content_variants_served_by_cache(&handles.cache_db, cache_id, &hashes).await;

    let meta = match mode {
        QueryMode::Pull => {
            PullMetadata::load(&handles.cache_db, &handles.db.config, &served, &[cache_id])
                .await
                .unwrap_or_default()
        }
        _ => PullMetadata::default(),
    };

    let expire = gradient_wire::messages::PRESIGN_TTL;
    let mut result: Vec<gradient_wire::types::CachedPath> = Vec::new();

    for (hash, path) in &hash_path_pairs {
        if let Some(row) = served.get(*hash) {
            result.push(
                build_cached_entry(
                    &handles.db.config,
                    &handles.db.storage.nar_storage,
                    path,
                    mode,
                    expire,
                    row,
                    &meta,
                )
                .await,
            );
        }
    }

    result
}

pub(super) async fn path_in_cache(db: &CacheDb, cache_id: CacheId, path: &StorePath) -> bool {
    !content_variants_served_by_cache(db, cache_id, &[path.hash().to_owned()])
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
    worker: &str,
    job: CacheQueryJob,
    paths: &[String],
    nar_sizes: &[Option<u64>],
    mode: gradient_wire::types::QueryMode,
    external: bool,
) -> Result<Vec<gradient_wire::types::CachedPath>, DbErr> {
    query(handles, worker, job, paths, nar_sizes, mode, external).await
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

    #[derive(Debug)]
    struct TrustedUpstreamOnly;

    impl WorkerJobPolicy for TrustedUpstreamOnly {
        fn allows(&self, worker: &str, job: WorkerJob<'_>) -> bool {
            worker != "restricted"
                || matches!(job, WorkerJob::Substitution { verifying_keys }
                    if verifying_keys.iter().all(|key| key.as_deref() == Some(TRUSTED_KEY)))
        }
    }

    const TRUSTED_KEY: &str = "trusted-1:t0t0";
    const A_OUT: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    const B_OUT: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

    #[tokio::test]
    async fn a_restricted_worker_hears_only_of_upstream_hits_its_job_policy_allows() {
        use gradient_entity::cache_upstream::CacheUpstreamKind;
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::shape::shape;

        let (trusted, other) = (CacheUpstreamId::now_v7(), CacheUpstreamId::now_v7());
        let db = shape()
            .evaluation(gradient_types::ids::EvaluationId::now_v7())
            .derivation("d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0-a.drv")
            .cache_available()
            .output(A_OUT)
            .verified_by(trusted)
            .derivation("d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1-b.drv")
            .cache_available()
            .output(B_OUT)
            .verified_by(other)
            .into_cache(CacheId::now_v7())
            .upstream(
                trusted,
                "https://trusted.invalid",
                TRUSTED_KEY,
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadWrite,
                true,
            )
            .upstream(
                other,
                "https://other.invalid",
                "other-1:o0o0",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadWrite,
                true,
            )
            .into_database()
            .await;
        let state = TestState::new()
            .worker_db(db.connection())
            .web_db(db.connection())
            .cache_db(db.connection())
            .worker_job_policy(Arc::new(TrustedUpstreamOnly))
            .build();
        let handles = SessionHandles::from_state(&state);
        let mut answered = Vec::new();
        for worker in ["restricted", "other"] {
            for (hash, name) in [(A_OUT, "a"), (B_OUT, "b")] {
                let path = format!("/nix/store/{hash}-{name}");
                let found = query(
                    &handles,
                    worker,
                    CacheQueryJob::for_test_project(ProjectId::nil()),
                    &[path],
                    &[],
                    QueryMode::Pull,
                    true,
                )
                .await
                .unwrap()
                .iter()
                .any(|c| c.cached && c.url.is_some());
                answered.push((worker, name, found));
            }
        }

        assert_eq!(
            answered,
            vec![
                ("restricted", "a", true),
                ("restricted", "b", false),
                ("other", "a", true),
                ("other", "b", true),
            ]
        );
        drop((handles, state));
        db.finish().await;
    }

    #[tokio::test]
    async fn worker_receives_persisted_nar_urls_only_from_active_upstream_caches_without_credential()
     {
        use gradient_entity::cache_upstream::{
            ActiveModel as ACacheUpstream, CacheUpstreamKind, UpstreamCredentialScheme,
        };
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::shape::shape;
        use sea_orm::ActiveValue::Set;

        const C_OUT: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
        const D_OUT: &str = "d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0";
        let (open, private, inactive) = (
            CacheUpstreamId::now_v7(),
            CacheUpstreamId::now_v7(),
            CacheUpstreamId::now_v7(),
        );
        let db = shape()
            .evaluation(gradient_types::ids::EvaluationId::now_v7())
            .derivation("brik07vzpr94765f88nfvnn6ip95vcnf-a.drv")
            .cache_available()
            .output(A_OUT)
            .verified_by(open)
            .derivation("a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5-b.drv")
            .cache_available()
            .output(B_OUT)
            .verified_by(private)
            .derivation("1if07p7njnc1w7airw4mwfnh5wrvf8sj-c.drv")
            .cache_available()
            .output(C_OUT)
            .verified_by(inactive)
            .derivation("njh5k7lsibz3r2r87zwsanl138fxqyr4-d.drv")
            .cache_available()
            .output(D_OUT)
            .into_cache(CacheId::now_v7())
            .upstream(
                open,
                "https://open.invalid",
                "open-1:o0o0",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadOnly,
                true,
            )
            .upstream(
                private,
                "https://private.invalid",
                "private-1:p0p0",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadOnly,
                true,
            )
            .upstream(
                inactive,
                "https://inactive.invalid",
                "inactive-1:i0i0",
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadOnly,
                false,
            )
            .into_database()
            .await;
        gradient_db::rows::update::<ECacheUpstream, _>(
            &db.connection(),
            ACacheUpstream {
                id: Set(private),
                encrypted_credential: Set(Some("c2VjcmV0LWNpcGhlcnRleHQ=".into())),
                credential_scheme: Set(Some(UpstreamCredentialScheme::Basic)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let state = TestState::new()
            .worker_db(db.connection())
            .web_db(db.connection())
            .cache_db(db.connection())
            .build();
        let handles = SessionHandles::from_state(&state);

        let mut answered = Vec::new();
        for (hash, name) in [(A_OUT, "a"), (B_OUT, "b"), (C_OUT, "c"), (D_OUT, "d")] {
            let path = format!("/nix/store/{hash}-{name}");
            let url = query(
                &handles,
                "worker",
                CacheQueryJob::for_test_project(ProjectId::nil()),
                &[path],
                &[],
                QueryMode::Pull,
                true,
            )
            .await
            .unwrap()
            .into_iter()
            .find_map(|c| c.url);
            answered.push((name, url));
        }

        assert_eq!(
            answered,
            vec![
                (
                    "a",
                    Some(format!("https://upstream.invalid/nar/{A_OUT}.nar").into())
                ),
                ("b", None),
                ("c", None),
                ("d", None),
            ]
        );
        drop((handles, state));
        db.finish().await;
    }

    #[tokio::test]
    async fn a_project_or_cache_is_answered_only_with_the_content_variant_it_trusts() {
        use gradient_fixtures::store_path_content_variants::{
            SECOND_KEY, SHARED, UNTRUSTED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        readers
            .db
            .execute("UPDATE cached_path SET file_size = 1099511627776")
            .await;
        let state = TestState::new()
            .worker_db(readers.db.connection())
            .web_db(readers.db.connection())
            .cache_db(readers.db.connection())
            .nar_storage(s3_nar_storage())
            .build();
        let handles = SessionHandles::from_state(&state);
        let shared = format!("/nix/store/{SHARED}-shared");
        let untrusted = format!("/nix/store/{UNTRUSTED}-untrusted");
        let ask = async |path: &str, mode: QueryMode| {
            query(
                &handles,
                "w1",
                CacheQueryJob::for_test_project(readers.project_b),
                &[path.to_owned()],
                &[Some(1)],
                mode,
                false,
            )
            .await
            .unwrap()
        };

        let pulled = ask(&shared, QueryMode::Pull).await;
        assert!(
            pulled.len() == 1
                && pulled[0].nar_hash.as_deref() == Some("sha256:second")
                && pulled[0]
                    .url
                    .as_ref().map(gradient_wire::types::UrlWithSecretQuery::as_full_url_with_access_credential_query)
                    .is_some_and(|url| url.contains(&SECOND_KEY[2..])),
            "Project B trusts only its own variant of the shared hash, so the answer carries the NAR hash and the download link of that variant: {pulled:?}"
        );
        let pulled_untrusted = ask(&untrusted, QueryMode::Pull).await;
        let pushed_untrusted = ask(&untrusted, QueryMode::Push).await;
        assert!(
            pulled_untrusted.is_empty()
                && pushed_untrusted.len() == 1
                && !pushed_untrusted[0].cached
                && pushed_untrusted[0].url.is_none(),
            "Project B trusts no variant of this hash, so a pull gets no entry and a push hears that the path is not cached: {pulled_untrusted:?} {pushed_untrusted:?}"
        );

        let mut served = Vec::new();
        for cache in [readers.cache_one, readers.cache_two] {
            let answer = query_for_cache(
                &handles,
                cache,
                std::slice::from_ref(&shared),
                QueryMode::Pull,
            )
            .await;
            served.push(answer.first().and_then(|entry| entry.nar_hash.clone()));
        }
        assert!(
            served[0].is_some()
                && served[0].as_deref() != Some("sha256:second")
                && served[1].as_deref() == Some("sha256:second"),
            "Each cache answers with the variant it serves: cache C1 with the variant of project A, cache C2 with the variant of project B: {served:?}"
        );
        drop((handles, state));
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn pull_answer_does_not_include_signature_of_other_projects_private_cache() {
        use gradient_fixtures::store_path_content_variants::{
            SECOND_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        readers
            .db
            .execute(&format!(
                "CREATE TEMP TABLE private_cache AS SELECT * FROM cache WHERE id = '{}'; \
                 UPDATE private_cache SET id = uuidv7(), name = 'private-cache-of-another-project'; \
                 INSERT INTO cache SELECT * FROM private_cache; \
                 INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
                 SELECT uuidv7(), signed.id, private_cache.id, convert_to('signature', 'UTF8'), 0, now() \
                 FROM cached_path signed, private_cache WHERE signed.nar_object_storage_key = '{SECOND_KEY}'",
                readers.cache_one,
            ))
            .await;
        let state = TestState::new()
            .worker_db(readers.db.connection())
            .web_db(readers.db.connection())
            .cache_db(readers.db.connection())
            .nar_storage(s3_nar_storage())
            .build();
        let handles = SessionHandles::from_state(&state);
        let shared = format!("/nix/store/{SHARED}-shared");

        let pulled_by_project_b = query(
            &handles,
            "w1",
            CacheQueryJob::for_test_project(readers.project_b),
            std::slice::from_ref(&shared),
            &[Some(1)],
            QueryMode::Pull,
            false,
        )
        .await
        .unwrap();
        let pulled_from_cache_two = query_for_cache(
            &handles,
            readers.cache_two,
            std::slice::from_ref(&shared),
            QueryMode::Pull,
        )
        .await;

        for answer in [&pulled_by_project_b, &pulled_from_cache_two] {
            let signatures = answer
                .first()
                .and_then(|entry| entry.signatures.clone())
                .unwrap_or_default();
            assert!(
                signatures.len() == 1
                    && !signatures
                        .iter()
                        .any(|signature| signature.contains("private-cache-of-another-project")),
                "Neither Project B nor cache C2 can read the private cache. Only the signatures of cache C2 appear in the answer: {signatures:?}"
            );
        }
        drop((handles, state));
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn a_push_query_with_mismatched_sizes_is_an_error() {
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        assert!(
            query(
                &SessionHandles::from_state(&state),
                "w1",
                CacheQueryJob::for_test_project(ProjectId::nil()),
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

    fn s3_nar_storage() -> gradient_storage::NarStore {
        gradient_storage::NarStore::s3(
            "bucket",
            "us-east-1",
            Some("http://127.0.0.1:1"),
            Some("key"),
            Some("secret"),
            "",
            false,
            gradient_storage::S3Timeouts::default(),
        )
        .expect("an S3 client builds and presigns without a network")
    }

    fn make_s3_state() -> Arc<ServerState> {
        cache_state_builder().nar_storage(s3_nar_storage()).build()
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
            "w1",
            CacheQueryJob::for_test_project(ProjectId::nil()),
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
                "w1",
                CacheQueryJob::for_test_project(ProjectId::nil()),
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
            "w1",
            CacheQueryJob::for_test_project(ProjectId::nil()),
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
                "w1",
                CacheQueryJob::for_test_project(ProjectId::nil()),
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
                "w1",
                CacheQueryJob::for_test_project(ProjectId::nil()),
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
                "w1",
                CacheQueryJob::for_test_project(ProjectId::nil()),
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
            "w1",
            CacheQueryJob::for_test_project(ProjectId::nil()),
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
            "w1",
            CacheQueryJob::for_test_project(ProjectId::nil()),
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
            "w1",
            CacheQueryJob::for_test_project(ProjectId::nil()),
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
