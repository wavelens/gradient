/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_types::ids::{CacheId, CachedPathId, OrganizationId};
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use tracing::{error, warn};

/// Look up the locally-cached `(file_size, nar_size)` for `hashes`.
///
/// A DB error is propagated, never swallowed into an empty map: a failed lookup
/// means cache state is *unknown*, and treating "unknown" as "absent" would make
/// a `CacheQuery` report a fully-cached input as missing - which the worker takes
/// as a terminal `InputsUnavailable` and fails the eval. The caller turns the
/// error into a `CacheError` so the worker retries transiently instead.
async fn build_local_cache_map(
    state: &ServerState,
    hashes: &[&str],
) -> Result<HashMap<String, (Option<i64>, Option<i64>)>, DbErr> {
    // Source the (file_size, nar_size) pair straight from `cached_path` -
    // the worker writes both columns there during NarUploaded, so it's the
    // single authoritative copy.  We still scope the result to outputs the
    // server marks `is_cached`, so an in-flight upload doesn't leak.
    let derivation_outputs = EDerivationOutput::find()
        .filter(
            sea_orm::Condition::all()
                .add(CDerivationOutput::IsCached.eq(true))
                .add(CDerivationOutput::Hash.is_in(hashes.to_vec())),
        )
        .all(&state.cache_db)
        .await?;

    let cached_paths = ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(hashes.to_vec()))
        .all(&state.cache_db)
        .await?;

    // Only paths whose NAR upload actually completed count as cached.
    // `is_fully_cached()` requires `file_hash IS NOT NULL`; rows without
    // it are placeholders for an in-flight or failed upload and would
    // cause the worker to issue a `NarRequest` the server can't satisfy.
    let sizes: HashMap<String, (Option<i64>, Option<i64>)> = cached_paths
        .into_iter()
        .filter(|cp| cp.is_fully_cached())
        .map(|cp| (cp.hash, (cp.file_size, cp.nar_size)))
        .collect();

    Ok(derivation_outputs
        .into_iter()
        .filter_map(|row| sizes.get(&row.hash).map(|sizes| (row.hash, *sizes)))
        .collect())
}

async fn load_cached_path_rows(
    state: &ServerState,
    hashes: &[&str],
) -> Result<Vec<gradient_entity::cached_path::Model>, DbErr> {
    ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(hashes.to_vec()))
        .all(&state.cache_db)
        .await
}

/// For Push mode: ensure a `cached_path_signature` row exists for each
/// (cached_path, org cache) pair so that the signing job is triggered for
/// paths that were already cached before this worker connected.
async fn ensure_push_signatures(
    state: &ServerState,
    org_id: OrganizationId,
    cached_path_rows: &[gradient_entity::cached_path::Model],
) {
    if cached_path_rows.is_empty() {
        return;
    }

    let org_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org_id))
        .all(&state.cache_db)
        .await
        .unwrap_or_default();
    if org_caches.is_empty() {
        return;
    }

    let cache_ids: Vec<uuid::Uuid> = org_caches.iter().map(|oc| oc.cache.into_inner()).collect();
    let path_ids: Vec<uuid::Uuid> = cached_path_rows.iter().map(|cp| cp.id.into_inner()).collect();

    // Insert via `SELECT FROM cached_path` (cross-joined with the org caches) so a
    // path concurrently purged between the lookup and here is simply skipped,
    // instead of failing the whole batch on the cached_path FK. Array params keep
    // each statement to two binds regardless of row count; chunk the path list so
    // a worker reconnecting with a large store does not insert millions at once.
    const SIGNATURE_PATH_BATCH: usize = 8000;

    for chunk in path_ids.chunks(SIGNATURE_PATH_BATCH) {
        let result = state
            .cache_db
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                INSERT INTO cached_path_signature (id, cached_path, cache, created_at)
                SELECT uuidv7(), cp.id, c.cache_id, (now() AT TIME ZONE 'UTC')
                FROM cached_path cp
                CROSS JOIN unnest($2::uuid[]) AS c(cache_id)
                WHERE cp.id = ANY($1::uuid[])
                ON CONFLICT (cached_path, cache) DO NOTHING
                "#,
                [chunk.to_vec().into(), cache_ids.clone().into()],
            ))
            .await;
        if let Err(e) = result {
            warn!(
                %org_id,
                error = %e,
                "failed to insert cached_path_signature placeholders (Push mode)"
            );
        }
    }
}

async fn load_cached_path_signatures(
    state: &ServerState,
    cached_path_id: CachedPathId,
    hash: &str,
) -> Option<Vec<String>> {
    let rows = match ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path_id))
        .all(&state.cache_db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(%hash, error = %e, "failed to load cached_path signatures");
            return None;
        }
    };

    let cache_ids: Vec<CacheId> = rows.iter().map(|r| r.cache).collect();
    let cache_names: HashMap<CacheId, String> = if cache_ids.is_empty() {
        HashMap::new()
    } else {
        ECache::find()
            .filter(CCache::Id.is_in(cache_ids))
            .all(&state.cache_db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.id, c.name))
            .collect()
    };

    let serve_url = &state.config.server.serve_url;
    let sigs: Vec<String> = rows
        .into_iter()
        .filter_map(|r| {
            let stored = r.signature?;
            let cache_name = cache_names.get(&r.cache)?;
            Some(gradient_sources::full_signature_token(
                &stored, serve_url, cache_name,
            ))
        })
        .collect();
    if sigs.is_empty() { None } else { Some(sigs) }
}

/// Resolve the import metadata a worker needs to construct a `ValidPathInfo`
/// and call `add_to_store_nar` on its local nix-daemon.
///
/// Returns `(None, None, None, None, None)` if no `cached_path` row exists.
async fn fetch_pull_metadata(
    state: &ServerState,
    hash: &str,
) -> (
    Option<String>,      // nar_hash
    Option<String>,      // file_hash
    Option<Vec<String>>, // references (full /nix/store/... paths)
    Option<Vec<String>>, // signatures (narinfo wire format)
    Option<String>,      // deriver
    Option<String>,      // ca
) {
    let cached_row = match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(&state.cache_db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return (None, None, None, None, None, None),
        Err(e) => {
            warn!(%hash, error = %e, "failed to load cached_path for Pull metadata");
            return (None, None, None, None, None, None);
        }
    };

    let reference_tokens = gradient_db::references_for_hash(&state.cache_db, hash)
        .await
        .unwrap_or_default();
    let references = (!reference_tokens.is_empty())
        .then(|| expand_references(Some(&reference_tokens.join(" "))))
        .flatten();
    let signatures = load_cached_path_signatures(state, cached_row.id, hash).await;

    (
        cached_row.nar_hash,
        cached_row.file_hash,
        references,
        signatures,
        cached_row.deriver,
        cached_row.ca,
    )
}

async fn build_cached_entry(
    state: &ServerState,
    hash: &str,
    path: &str,
    file_size: &Option<i64>,
    nar_size: &Option<i64>,
    mode: gradient_types::proto::QueryMode,
    expire: std::time::Duration,
) -> gradient_types::proto::CachedPath {
    use gradient_types::proto::{CachedPath, QueryMode};

    // Push mode carries only `path` + `cached`. No URL, no metadata.
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

    let (url, nar_hash, file_hash, references, signatures, deriver, ca) = match mode {
        QueryMode::Pull => {
            let url = match state.nar_storage.presigned_get_url(hash, expire).await {
                Ok(u) => u,
                Err(e) => {
                    warn!(%hash, error = %e, "failed to generate presigned GET URL");
                    None
                }
            };
            let meta = fetch_pull_metadata(state, hash).await;
            (url, meta.0, meta.1, meta.2, meta.3, meta.4, meta.5)
        }
        _ => (None, None, None, None, None, None, None),
    };

    CachedPath {
        path: path.to_string(),
        cached: true,
        file_size: file_size.map(|v| v as u64),
        nar_size: nar_size.map(|v| v as u64),
        url,
        nar_hash,
        file_hash,
        references,
        signatures,
        deriver,
        ca,
    }
}

/// Push-mode response for an uncached path: `path` + `cached: false` plus
/// a presigned PUT URL when the NAR store supports one (S3). Local stores
/// return `url: None` and the worker uploads via `NarPush` over the
/// WebSocket. No metadata, no upstream lookup.
async fn build_uncached_push_entry(
    state: &ServerState,
    hash: &str,
    path: &str,
    expire: std::time::Duration,
) -> gradient_types::proto::CachedPath {
    use gradient_types::proto::CachedPath;

    let url = match state.nar_storage.presigned_put_url(hash, expire).await {
        Ok(u) => u,
        Err(e) => {
            error!(
                %hash,
                error = %e,
                "S3 presigned PUT URL generation failed; worker will fall back to \
                 direct NarPush (this defeats S3 - check S3 credentials / endpoint / \
                 region config)"
            );
            None
        }
    };

    CachedPath {
        path: path.to_string(),
        cached: false,
        file_size: None,
        nar_size: None,
        url,
        nar_hash: None,
        file_hash: None,
        references: None,
        signatures: None,
        deriver: None,
        ca: None,
    }
}

/// Serve upstream availability resolved once at eval time and persisted onto
/// `derivation_output` (`external_url` + narinfo metadata). Extends `result`
/// with a `CachedPath` pointing at the persisted upstream URL and returns the
/// set of hashes served, so the live narinfo lookup is skipped for them.
async fn extend_with_persisted_upstream(
    state: &ServerState,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_types::proto::CachedPath>,
) -> HashSet<String> {
    let mut served = HashSet::new();
    if uncached_pairs.is_empty() {
        return served;
    }

    let hashes: Vec<String> = uncached_pairs.iter().map(|(h, _)| h.clone()).collect();
    let rows = match EDerivationOutput::find()
        .filter(CDerivationOutput::Hash.is_in(hashes))
        .filter(CDerivationOutput::ExternalUrl.is_not_null())
        .all(&state.cache_db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "persisted upstream lookup failed");
            return served;
        }
    };

    // Content-addressed, so any row sharing a hash carries an equivalent entry.
    let by_hash: HashMap<String, gradient_entity::derivation_output::Model> =
        rows.into_iter().map(|r| (r.hash.clone(), r)).collect();

    for (hash, path) in uncached_pairs {
        let Some(row) = by_hash.get(hash) else {
            continue;
        };
        let Some(url) = row.external_url.clone() else {
            continue;
        };
        result.push(gradient_types::proto::CachedPath {
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

/// Probe org-configured upstream caches for any `uncached_pairs` not found
/// locally, extending `result` with any hits. Outbound probes are bounded by
/// the shared upstream-query semaphore. No-ops if the org has no upstream URLs.
async fn extend_with_upstream_results(
    state: &ServerState,
    org_id: OrganizationId,
    uncached_pairs: Vec<(String, String)>,
    result: &mut Vec<gradient_types::proto::CachedPath>,
) {
    const UPSTREAM_WINDOW_MINUTES: i64 = 60;

    let endpoints =
        match gradient_db::upstream_endpoints_for_org(&state.cache_db, org_id, UPSTREAM_WINDOW_MINUTES)
            .await
        {
            Ok(eps) => eps,
            Err(e) => {
                warn!(%org_id, error = %e, "CacheQuery upstream lookup failed");
                return;
            }
        };
    if endpoints.is_empty() {
        return;
    }

    let (found, stats) = gradient_core::upstream::probe_batch(
        state.http.clone(),
        endpoints,
        std::sync::Arc::clone(&state.upstream_query),
        uncached_pairs,
    )
    .await;

    for (_hash, cp) in found {
        result.push(cp);
    }

    let bucket = {
        use chrono::Timelike as _;
        let now = gradient_types::now();
        now.with_second(0)
            .and_then(|t: chrono::NaiveDateTime| t.with_nanosecond(0))
            .unwrap_or(now)
    };
    if let Err(e) = gradient_db::upsert_upstream_metrics(&state.cache_db, bucket, &stats).await {
        warn!(error = %e, "failed to flush upstream metrics");
    }
}

/// Pull availability for any still-uncached paths from the org's configured
/// gradient_proto upstreams, extending `result` with hits.
async fn extend_with_gradient_proto_results(
    state: &ServerState,
    org_id: OrganizationId,
    uncached_pairs: &[(String, String)],
    result: &mut Vec<gradient_types::proto::CachedPath>,
) {
    let upstreams =
        match gradient_db::gradient_proto_upstreams_for_org(&state.cache_db, org_id).await {
            Ok(u) => u,
            Err(e) => {
                warn!(%org_id, error = %e, "gradient_proto upstream lookup failed");
                return;
            }
        };
    if upstreams.is_empty() {
        return;
    }

    let already: HashSet<String> = result.iter().map(|c| c.path.clone()).collect();
    let want: Vec<String> = uncached_pairs
        .iter()
        .map(|(_, p)| p.clone())
        .filter(|p| !already.contains(p))
        .collect();
    if want.is_empty() {
        return;
    }

    for up in upstreams {
        let api_key = up.api_key_enc.as_deref().and_then(|enc| {
            gradient_sources::decrypt_secret(&state.config.secrets.crypt_secret_file, enc).ok()
        });
        let found =
            super::cache_consumer::pull_paths(&up.url, &up.remote_cache, api_key.as_deref(), &want)
                .await;
        for cp in found {
            if !result.iter().any(|r| r.path == cp.path) {
                result.push(cp);
            }
        }
    }
}

/// Check which store paths are available - in the local Gradient cache or upstream.
///
/// Behaviour depends on `mode`:
/// - `Normal` - return only locally-cached paths; probe upstream for misses.
/// - `Pull`   - same as Normal but cached paths include a presigned S3 GET URL.
/// - `Push`   - return **all** queried paths with `cached` set; skip upstream.
///   Uncached paths include a presigned S3 PUT URL when S3-backed.
async fn query(
    state: &ServerState,
    org_id: Option<OrganizationId>,
    paths: &[String],
    mode: gradient_types::proto::QueryMode,
) -> Result<Vec<gradient_types::proto::CachedPath>, DbErr> {
    use gradient_types::proto::QueryMode;

    let hash_path_pairs: Vec<(&str, &str)> = paths
        .iter()
        .filter_map(|p| {
            let base = p.strip_prefix("/nix/store/").unwrap_or(p);
            let hash = base.split('-').next()?;
            if hash.len() == 32 {
                Some((hash, p.as_str()))
            } else {
                None
            }
        })
        .collect();

    if hash_path_pairs.is_empty() {
        return Ok(vec![]);
    }

    let hashes: Vec<&str> = hash_path_pairs.iter().map(|(h, _)| *h).collect();

    let cached_map = build_local_cache_map(state, &hashes).await?;
    let cached_path_rows = load_cached_path_rows(state, &hashes).await?;

    // Merge source-path cache hits into the map (keyed by hash string).
    let mut cached_map = cached_map;
    for cp in &cached_path_rows {
        if cp.is_fully_cached() {
            cached_map
                .entry(cp.hash.clone())
                .or_insert((cp.file_size, cp.nar_size));
        }
    }

    if matches!(mode, QueryMode::Push)
        && let Some(oid) = org_id
    {
        ensure_push_signatures(state, oid, &cached_path_rows).await;
    }

    let expire = std::time::Duration::from_secs(3600);
    let mut result: Vec<gradient_types::proto::CachedPath> = Vec::new();

    for (hash, path) in &hash_path_pairs {
        if let Some((file_size, nar_size)) = cached_map.get(*hash) {
            result.push(
                build_cached_entry(state, hash, path, file_size, nar_size, mode.clone(), expire)
                    .await,
            );
        } else if matches!(mode, QueryMode::Push) {
            result.push(build_uncached_push_entry(state, hash, path, expire).await);
        }
    }

    if matches!(mode, QueryMode::Push) {
        return Ok(result);
    }

    let locally_cached_hashes: std::collections::HashSet<&str> =
        cached_map.keys().map(|s| s.as_str()).collect();
    let uncached_pairs: Vec<(String, String)> = hash_path_pairs
        .iter()
        .filter(|(hash, _)| !locally_cached_hashes.contains(hash))
        .map(|(h, p)| (h.to_string(), p.to_string()))
        .collect();

    // Serve upstream availability resolved once at eval time
    // (`derivation_output.external_url` + narinfo metadata): the worker downloads
    // directly from the persisted URL, so the narinfo lookup is not re-run here.
    let resolved = extend_with_persisted_upstream(state, &uncached_pairs, &mut result).await;
    let uncached_pairs: Vec<(String, String)> = uncached_pairs
        .into_iter()
        .filter(|(h, _)| !resolved.contains(h))
        .collect();

    if !uncached_pairs.is_empty()
        && let Some(oid) = org_id
    {
        extend_with_upstream_results(state, oid, uncached_pairs.clone(), &mut result).await;
        extend_with_gradient_proto_results(state, oid, &uncached_pairs, &mut result).await;
    }

    if matches!(mode, QueryMode::Pull) {
        let returned: std::collections::HashSet<String> =
            result.iter().map(|cp| cp.path.clone()).collect();
        let missing: Vec<gradient_types::proto::CachedPath> = hash_path_pairs
            .iter()
            .filter(|(_, p)| !returned.contains(*p))
            .map(|(_, p)| build_uncached_pull_entry(p))
            .collect();
        result.extend(missing);
    }

    Ok(result)
}

/// Pull-mode response for a path the server cannot serve (neither in the
/// local cache nor in any configured upstream). Carries `cached: false` and
/// no metadata so the worker's prefetch hard-fail can distinguish "server has
/// nothing for this path" from "this path was never queried".
fn build_uncached_pull_entry(path: &str) -> gradient_types::proto::CachedPath {
    use gradient_types::proto::CachedPath;
    CachedPath {
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

/// Return the subset of `hashes` that have a `cached_path_signature` row for
/// `cache_id`. Fails closed: any DB error yields an empty set so a public
/// cache never leaks paths it cannot prove belong to it.
async fn hashes_in_cache(
    state: &ServerState,
    cache_id: CacheId,
    hashes: &[&str],
) -> HashSet<String> {
    if hashes.is_empty() {
        return HashSet::new();
    }

    let cached_paths = match ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(hashes.to_vec()))
        .all(&state.cache_db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "cache-scoped query cached_path lookup failed");
            return HashSet::new();
        }
    };

    let id_to_hash: HashMap<CachedPathId, String> =
        cached_paths.into_iter().map(|cp| (cp.id, cp.hash)).collect();
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
        .all(&state.cache_db)
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

/// Read-only query scoped to a single cache. Only paths with a
/// `cached_path_signature` row for `cache_id` are returned. `Push` is never
/// honored - it would mint upload URLs and is meaningless for a public cache.
pub(super) async fn query_for_cache(
    state: &ServerState,
    cache_id: CacheId,
    paths: &[String],
    mode: gradient_types::proto::QueryMode,
) -> Vec<gradient_types::proto::CachedPath> {
    use gradient_types::proto::QueryMode;

    if matches!(mode, QueryMode::Push) {
        return vec![];
    }

    let hash_path_pairs: Vec<(&str, &str)> = paths
        .iter()
        .filter_map(|p| {
            let base = p.strip_prefix("/nix/store/").unwrap_or(p);
            let hash = base.split('-').next()?;
            if hash.len() == 32 {
                Some((hash, p.as_str()))
            } else {
                None
            }
        })
        .collect();

    if hash_path_pairs.is_empty() {
        return vec![];
    }

    let hashes: Vec<&str> = hash_path_pairs.iter().map(|(h, _)| *h).collect();

    let in_cache = hashes_in_cache(state, cache_id, &hashes).await;
    // Cache-serve endpoint: a DB error degrades to a miss (the consumer falls
    // back to its other sources), matching `hashes_in_cache`'s fail-closed
    // behaviour. Only the build-prefetch `query` path propagates the error.
    let cached_map = build_local_cache_map(state, &hashes)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "cache-serve local lookup failed; treating as miss");
            HashMap::new()
        });

    let expire = std::time::Duration::from_secs(3600);
    let mut result: Vec<gradient_types::proto::CachedPath> = Vec::new();

    for (hash, path) in &hash_path_pairs {
        if !in_cache.contains(*hash) {
            continue;
        }
        if let Some((file_size, nar_size)) = cached_map.get(*hash) {
            result.push(
                build_cached_entry(state, hash, path, file_size, nar_size, mode.clone(), expire)
                    .await,
            );
        }
    }

    if matches!(mode, QueryMode::Pull) {
        let returned: HashSet<String> = result.iter().map(|cp| cp.path.clone()).collect();
        let missing: Vec<gradient_types::proto::CachedPath> = hash_path_pairs
            .iter()
            .filter(|(_, p)| !returned.contains(*p))
            .map(|(_, p)| build_uncached_pull_entry(p))
            .collect();
        result.extend(missing);
    }

    result
}

/// Authorize a single store path against `cache_id`: true only when the path
/// has a `cached_path_signature` row for this cache.
pub(super) async fn path_in_cache(
    state: &ServerState,
    cache_id: CacheId,
    store_path: &str,
) -> bool {
    let base = store_path.strip_prefix("/nix/store/").unwrap_or(store_path);
    let Some(hash) = base.split('-').next() else {
        return false;
    };
    if hash.len() != 32 {
        return false;
    }
    !hashes_in_cache(state, cache_id, &[hash]).await.is_empty()
}

pub(super) async fn handle_cache_query(
    state: &ServerState,
    org_id: Option<OrganizationId>,
    paths: &[String],
    mode: gradient_types::proto::QueryMode,
) -> Result<Vec<gradient_types::proto::CachedPath>, DbErr> {
    query(state, org_id, paths, mode).await
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
    use std::sync::Arc;
    use gradient_types::proto::QueryMode;

    fn make_state() -> ServerState {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        // The CacheQuery handler reads `cache_db`, so drive lookups from there.
        Arc::try_unwrap(gradient_test_support::prelude::test_state_cache(db)).unwrap()
    }

    #[tokio::test]
    async fn cache_query_propagates_db_error_as_err() {
        // A DB error (e.g. pool exhaustion) must surface as Err so the handler
        // replies CacheError and the worker retries transiently - never be
        // swallowed into an empty/uncached list, which the worker would take as a
        // terminal InputsUnavailable on a fully-cached input.
        use sea_orm::{DbErr, RuntimeErr};
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors(vec![DbErr::Conn(RuntimeErr::Internal(
                "Connection pool timed out".to_string(),
            ))])
            .into_connection();
        let state =
            Arc::try_unwrap(gradient_test_support::prelude::test_state_cache(db)).unwrap();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        assert!(
            query(&state, None, &paths, QueryMode::Pull).await.is_err(),
            "DB error must propagate as Err, not a confident uncached result"
        );
    }

    #[tokio::test]
    async fn cache_query_empty_paths_returns_empty() {
        let state = make_state();

        assert!(query(&state, None, &[], QueryMode::Normal).await.unwrap().is_empty());
        assert!(query(&state, None, &[], QueryMode::Push).await.unwrap().is_empty());
        assert!(query(&state, None, &[], QueryMode::Pull).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cache_query_invalid_store_paths_skipped() {
        let state = make_state();

        let paths = vec![
            "not-a-store-path".to_string(),
            "/nix/store/short-name".to_string(),
        ];
        assert!(
            query(&state, None, &paths, QueryMode::Normal)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            query(&state, None, &paths, QueryMode::Push)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            query(&state, None, &paths, QueryMode::Pull)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cache_query_normal_uncached_returns_empty() {
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello".to_string()];
        let result = query(&state, None, &paths, QueryMode::Normal).await.unwrap();
        assert!(
            result.is_empty(),
            "Normal mode should not return uncached paths"
        );
    }

    #[tokio::test]
    async fn cache_query_pull_uncached_returns_entries_with_cached_false() {
        // Pull mode must surface every queried path, even ones the server cannot
        // serve. Without an explicit `cached: false` entry the worker has no way
        // to distinguish "server omitted this path" from "this path was never
        // queried", so its closure-walk hard-fail is bypassed and the build
        // proceeds to import a dependent path with an unsatisfiable reference,
        // surfacing as a confusing `daemon add_to_store_nar … path '…' is not
        // valid` error instead of the intended `not available in the gradient
        // cache` message.
        let state = make_state();
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar".to_string(),
        ];
        let result = query(&state, None, &paths, QueryMode::Pull).await.unwrap();
        assert_eq!(result.len(), 2, "Pull must return all queried paths");
        for cp in &result {
            assert!(!cp.cached, "uncached path: {}", cp.path);
            assert!(cp.url.is_none(), "Pull-uncached carries no URL");
            assert!(cp.nar_hash.is_none(), "Pull-uncached carries no nar_hash");
            assert!(
                cp.references.is_none(),
                "Pull-uncached carries no references"
            );
            assert!(
                cp.signatures.is_none(),
                "Pull-uncached carries no signatures"
            );
            assert!(cp.deriver.is_none(), "Pull-uncached carries no deriver");
            assert!(cp.ca.is_none(), "Pull-uncached carries no ca");
            assert!(cp.file_size.is_none(), "Pull-uncached carries no file_size");
            assert!(cp.nar_size.is_none(), "Pull-uncached carries no nar_size");
        }
        let returned: Vec<&str> = result.iter().map(|c| c.path.as_str()).collect();
        assert!(returned.contains(&paths[0].as_str()));
        assert!(returned.contains(&paths[1].as_str()));
    }

    #[tokio::test]
    async fn cache_query_push_uncached_returns_all_with_cached_false() {
        let state = make_state();
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar".to_string(),
        ];
        let result = query(&state, None, &paths, QueryMode::Push).await.unwrap();
        assert_eq!(result.len(), 2, "Push should return all queried paths");
        for cp in &result {
            assert!(!cp.cached, "all should be uncached (empty DB): {}", cp.path);
            // Local NAR storage returns None for presigned PUT (no S3); Push
            // with S3 would populate `url` - that's the only field Push may set
            // beyond `path` + `cached`.
            assert!(cp.url.is_none(), "local store → no presigned URL");
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
    async fn cache_query_rejects_overlong_hash() {
        // Hash component of 33 chars must be rejected - nix-base32 hashes are
        // exactly 32 chars. Guards against an `== 32` → `>= 32` length-check
        // relaxation.
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        for mode in [QueryMode::Normal, QueryMode::Pull, QueryMode::Push] {
            let result = query(&state, None, &paths, mode).await.unwrap();
            assert!(result.is_empty(), "33-char hash must be filtered out");
        }
    }

    #[tokio::test]
    async fn cache_query_push_deduplicates_by_hash() {
        let state = make_state();
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
        ];
        let result = query(&state, None, &paths, QueryMode::Push).await.unwrap();
        for cp in &result {
            assert!(!cp.cached);
        }
    }

    #[tokio::test]
    async fn cache_scoped_query_rejects_push() {
        let state = make_state();
        let cache = CacheId::new(uuid::Uuid::now_v7());
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        assert!(
            query_for_cache(&state, cache, &paths, QueryMode::Push)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cache_scoped_query_empty_returns_empty() {
        let state = make_state();
        let cache = CacheId::new(uuid::Uuid::now_v7());
        assert!(
            query_for_cache(&state, cache, &[], QueryMode::Normal)
                .await
                .is_empty()
        );
    }

    // ── expand_references ────────────────────────────────────────────────────

    #[test]
    fn expand_references_none_passthrough() {
        assert_eq!(expand_references(None), None);
    }

    #[test]
    fn expand_references_empty_string_empty_vec() {
        assert_eq!(expand_references(Some("")), Some(vec![]));
    }

    #[test]
    fn expand_references_splits_whitespace() {
        let out = expand_references(Some("aaaa-a bbbb-b cccc-c")).unwrap();
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn expand_references_prefixes_bare_names() {
        let out = expand_references(Some("aaaa-a")).unwrap();
        assert_eq!(out, vec!["/nix/store/aaaa-a".to_string()]);
    }

    #[test]
    fn expand_references_preserves_absolute() {
        let out = expand_references(Some("/nix/store/aaaa-a")).unwrap();
        assert_eq!(out, vec!["/nix/store/aaaa-a".to_string()]);
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
        // split_whitespace collapses runs of spaces/tabs.
        let out = expand_references(Some("aaaa-a   \t bbbb-b")).unwrap();
        assert_eq!(out.len(), 2);
    }
}
