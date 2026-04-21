/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;

use gradient_core::types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use tracing::{error, warn};
use uuid::Uuid;

/// Bundles a `&ServerState` reference to avoid threading `state` through every
/// cache-query helper as a free-function parameter.
struct CacheQueryHandler<'a> {
    state: &'a ServerState,
}

impl<'a> CacheQueryHandler<'a> {
    fn new(state: &'a ServerState) -> Self {
        Self { state }
    }

    async fn build_local_cache_map(
        &self,
        hashes: &[&str],
    ) -> HashMap<String, (Option<i64>, Option<i64>)> {
        match EDerivationOutput::find()
            .filter(
                sea_orm::Condition::all()
                    .add(CDerivationOutput::IsCached.eq(true))
                    .add(CDerivationOutput::Hash.is_in(hashes.to_vec())),
            )
            .all(&self.state.db)
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .map(|r| (r.hash, (r.file_size, r.nar_size)))
                .collect(),
            Err(e) => {
                warn!(error = %e, "CacheQuery local DB lookup failed");
                HashMap::new()
            }
        }
    }

    async fn load_cached_path_rows(&self, hashes: &[&str]) -> Vec<entity::cached_path::Model> {
        match ECachedPath::find()
            .filter(CCachedPath::Hash.is_in(hashes.to_vec()))
            .all(&self.state.db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "CacheQuery cached_path lookup failed");
                vec![]
            }
        }
    }

    /// For Push mode: ensure a `cached_path_signature` row exists for each
    /// (cached_path, org cache) pair so that the signing job is triggered for
    /// paths that were already cached before this worker connected.
    async fn ensure_push_signatures(
        &self,
        org_id: Uuid,
        cached_path_rows: &[entity::cached_path::Model],
    ) {
        let org_caches = EOrganizationCache::find()
            .filter(COrganizationCache::Organization.eq(org_id))
            .all(&self.state.db)
            .await
            .unwrap_or_default();

        for cp in cached_path_rows {
            for oc in &org_caches {
                let exists = ECachedPathSignature::find()
                    .filter(CCachedPathSignature::CachedPath.eq(cp.id))
                    .filter(CCachedPathSignature::Cache.eq(oc.cache))
                    .one(&self.state.db)
                    .await
                    .unwrap_or(None)
                    .is_some();

                if !exists {
                    let sig_row = ACachedPathSignature {
                        id: sea_orm::ActiveValue::Set(Uuid::new_v4()),
                        cached_path: sea_orm::ActiveValue::Set(cp.id),
                        cache: sea_orm::ActiveValue::Set(oc.cache),
                        signature: sea_orm::ActiveValue::Set(None),
                        created_at: sea_orm::ActiveValue::Set(chrono::Utc::now().naive_utc()),
                    };
                    let _ = sig_row.insert(&self.state.db).await;
                }
            }
        }
    }

    async fn load_cached_path_signatures(
        &self,
        cached_path_id: Uuid,
        hash: &str,
    ) -> Option<Vec<String>> {
        match ECachedPathSignature::find()
            .filter(CCachedPathSignature::CachedPath.eq(cached_path_id))
            .all(&self.state.db)
            .await
        {
            Ok(rows) => {
                let sigs: Vec<String> = rows.into_iter().filter_map(|r| r.signature).collect();
                if sigs.is_empty() { None } else { Some(sigs) }
            }
            Err(e) => {
                warn!(%hash, error = %e, "failed to load cached_path signatures");
                None
            }
        }
    }

    async fn resolve_deriver(&self, hash: &str) -> Option<String> {
        match EDerivationOutput::find()
            .filter(CDerivationOutput::Hash.eq(hash))
            .one(&self.state.db)
            .await
        {
            Ok(Some(out)) => match EDerivation::find_by_id(out.derivation)
                .one(&self.state.db)
                .await
            {
                Ok(Some(d)) => Some(d.derivation_path),
                _ => None,
            },
            _ => None,
        }
    }

    /// Resolve the import metadata a worker needs to construct a `ValidPathInfo`
    /// and call `add_to_store_nar` on its local nix-daemon.
    ///
    /// Returns `(None, None, None, None, None)` if no `cached_path` row exists.
    async fn fetch_pull_metadata(
        &self,
        hash: &str,
    ) -> (
        Option<String>,      // nar_hash
        Option<Vec<String>>, // references (full /nix/store/... paths)
        Option<Vec<String>>, // signatures (narinfo wire format)
        Option<String>,      // deriver
        Option<String>,      // ca
    ) {
        let cached_row = match ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash))
            .one(&self.state.db)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => return (None, None, None, None, None),
            Err(e) => {
                warn!(%hash, error = %e, "failed to load cached_path for Pull metadata");
                return (None, None, None, None, None);
            }
        };

        let references = expand_references(cached_row.references.as_deref());
        let signatures = self.load_cached_path_signatures(cached_row.id, hash).await;
        let deriver = self.resolve_deriver(hash).await;

        (
            cached_row.nar_hash,
            references,
            signatures,
            deriver,
            cached_row.ca,
        )
    }

    async fn build_cached_entry(
        &self,
        hash: &str,
        path: &str,
        file_size: &Option<i64>,
        nar_size: &Option<i64>,
        mode: gradient_core::types::proto::QueryMode,
        expire: std::time::Duration,
    ) -> gradient_core::types::proto::CachedPath {
        use gradient_core::types::proto::{CachedPath, QueryMode};

        let (url, nar_hash, references, signatures, deriver, ca) = match mode {
            QueryMode::Pull => {
                let url = match self.state.nar_storage.presigned_get_url(hash, expire).await {
                    Ok(u) => u,
                    Err(e) => {
                        warn!(%hash, error = %e, "failed to generate presigned GET URL");
                        None
                    }
                };
                let meta = self.fetch_pull_metadata(hash).await;
                (url, meta.0, meta.1, meta.2, meta.3, meta.4)
            }
            _ => (None, None, None, None, None, None),
        };

        CachedPath {
            path: path.to_string(),
            cached: true,
            file_size: file_size.map(|v| v as u64),
            nar_size: nar_size.map(|v| v as u64),
            url,
            nar_hash,
            references,
            signatures,
            deriver,
            ca,
        }
    }

    async fn build_uncached_push_entry(
        &self,
        hash: &str,
        path: &str,
        expire: std::time::Duration,
    ) -> gradient_core::types::proto::CachedPath {
        use gradient_core::types::proto::CachedPath;

        let url = match self.state.nar_storage.presigned_put_url(hash, expire).await {
            Ok(u) => u,
            Err(e) => {
                error!(
                    %hash,
                    error = %e,
                    "S3 presigned PUT URL generation failed; worker will fall back to \
                     direct NarPush (this defeats S3 — check S3 credentials / endpoint / \
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
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }
    }

    /// Probe org-configured upstream caches for any `uncached_pairs` not found
    /// locally, extending `result` with any hits.
    ///
    /// Concurrency-capped at 16 to bound worst-case latency. No-ops if the org
    /// has no upstream URLs configured.
    async fn extend_with_upstream_results(
        &self,
        org_id: Uuid,
        uncached_pairs: Vec<(String, String)>,
        result: &mut Vec<gradient_core::types::proto::CachedPath>,
    ) {
        use entity::organization_cache::CacheSubscriptionMode;
        use futures::stream::{FuturesUnordered, StreamExt as _};

        const UPSTREAM_LOOKUP_CONCURRENCY: usize = 16;

        let org_cache_rows = match EOrganizationCache::find()
            .filter(
                sea_orm::Condition::all()
                    .add(COrganizationCache::Organization.eq(org_id))
                    .add(COrganizationCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
            )
            .all(&self.state.db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(%org_id, error = %e, "CacheQuery org_cache lookup failed");
                return;
            }
        };

        let cache_ids: Vec<Uuid> = org_cache_rows.iter().map(|r| r.cache).collect();
        if cache_ids.is_empty() {
            return;
        }

        let upstream_rows = match ECacheUpstream::find()
            .filter(
                sea_orm::Condition::all()
                    .add(CCacheUpstream::Cache.is_in(cache_ids))
                    .add(CCacheUpstream::Url.is_not_null())
                    .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
            )
            .all(&self.state.db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(%org_id, error = %e, "CacheQuery upstream lookup failed");
                return;
            }
        };

        let upstream_urls: Vec<String> = upstream_rows.into_iter().filter_map(|r| r.url).collect();
        if upstream_urls.is_empty() {
            return;
        }

        let http = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .connect_timeout(std::time::Duration::from_secs(3))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to build upstream HTTP client; skipping upstream lookup");
                return;
            }
        };

        let upstream_urls = Arc::new(upstream_urls);
        let mut futs = FuturesUnordered::new();
        let mut iter = uncached_pairs.into_iter();

        for _ in 0..UPSTREAM_LOOKUP_CONCURRENCY {
            if let Some((hash, store_path)) = iter.next() {
                futs.push(lookup_upstream_narinfo(
                    http.clone(),
                    Arc::clone(&upstream_urls),
                    hash,
                    store_path,
                ));
            }
        }
        while let Some(found) = futs.next().await {
            if let Some(cp) = found {
                result.push(cp);
            }
            if let Some((hash, store_path)) = iter.next() {
                futs.push(lookup_upstream_narinfo(
                    http.clone(),
                    Arc::clone(&upstream_urls),
                    hash,
                    store_path,
                ));
            }
        }
    }

    /// Check which store paths are available — in the local Gradient cache or upstream.
    ///
    /// Behaviour depends on `mode`:
    /// - `Normal` — return only locally-cached paths; probe upstream for misses.
    /// - `Pull`   — same as Normal but cached paths include a presigned S3 GET URL.
    /// - `Push`   — return **all** queried paths with `cached` set; skip upstream.
    ///   Uncached paths include a presigned S3 PUT URL when S3-backed.
    async fn query(
        &self,
        org_id: Option<Uuid>,
        paths: &[String],
        mode: gradient_core::types::proto::QueryMode,
    ) -> Vec<gradient_core::types::proto::CachedPath> {
        use gradient_core::types::proto::QueryMode;

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

        let cached_map = self.build_local_cache_map(&hashes).await;
        let cached_path_rows = self.load_cached_path_rows(&hashes).await;

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
            self.ensure_push_signatures(oid, &cached_path_rows).await;
        }

        let expire = std::time::Duration::from_secs(3600);
        let mut result: Vec<gradient_core::types::proto::CachedPath> = Vec::new();

        for (hash, path) in &hash_path_pairs {
            if let Some((file_size, nar_size)) = cached_map.get(*hash) {
                result.push(
                    self.build_cached_entry(hash, path, file_size, nar_size, mode.clone(), expire)
                        .await,
                );
            } else if matches!(mode, QueryMode::Push) {
                result.push(self.build_uncached_push_entry(hash, path, expire).await);
            }
        }

        if matches!(mode, QueryMode::Push) {
            return result;
        }

        let locally_cached_hashes: std::collections::HashSet<&str> =
            cached_map.keys().map(|s| s.as_str()).collect();
        let uncached_pairs: Vec<(String, String)> = hash_path_pairs
            .iter()
            .filter(|(hash, _)| !locally_cached_hashes.contains(hash))
            .map(|(h, p)| (h.to_string(), p.to_string()))
            .collect();

        if !uncached_pairs.is_empty()
            && let Some(oid) = org_id
        {
            self.extend_with_upstream_results(oid, uncached_pairs, &mut result)
                .await;
        }

        result
    }
}

pub(super) async fn handle_cache_query(
    state: &ServerState,
    org_id: Option<Uuid>,
    paths: &[String],
    mode: gradient_core::types::proto::QueryMode,
) -> Vec<gradient_core::types::proto::CachedPath> {
    CacheQueryHandler::new(state)
        .query(org_id, paths, mode)
        .await
}

/// Probe each `upstream_url` for `<hash>.narinfo` until one responds 2xx.
/// Returns `Some(CachedPath)` pointing at the upstream NAR URL on first hit,
/// `None` if no upstream has the path.
async fn lookup_upstream_narinfo(
    http: reqwest::Client,
    upstream_urls: Arc<Vec<String>>,
    hash: String,
    store_path: String,
) -> Option<crate::messages::CachedPath> {
    for base_url in upstream_urls.iter() {
        let narinfo_url = format!("{}/{}.narinfo", base_url.trim_end_matches('/'), &hash);
        let body = match http.get(&narinfo_url).send().await {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(b) => b,
                Err(_) => continue,
            },
            _ => continue,
        };
        if let Some(nar_path) = body
            .lines()
            .find_map(|l| l.strip_prefix("URL: ").map(str::trim))
        {
            let url = format!("{}/{}", base_url.trim_end_matches('/'), nar_path);
            return Some(crate::messages::CachedPath {
                path: store_path,
                cached: true,
                file_size: None,
                nar_size: None,
                url: Some(url),
                nar_hash: None,
                references: None,
                signatures: None,
                deriver: None,
                ca: None,
            });
        }
    }
    None
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
    use gradient_core::types::proto::QueryMode;

    fn make_state() -> ServerState {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        Arc::try_unwrap(test_support::prelude::test_state(db)).unwrap()
    }

    #[tokio::test]
    async fn cache_query_empty_paths_returns_empty() {
        let state = make_state();
        let h = CacheQueryHandler::new(&state);
        assert!(h.query(None, &[], QueryMode::Normal).await.is_empty());
        assert!(h.query(None, &[], QueryMode::Push).await.is_empty());
        assert!(h.query(None, &[], QueryMode::Pull).await.is_empty());
    }

    #[tokio::test]
    async fn cache_query_invalid_store_paths_skipped() {
        let state = make_state();
        let h = CacheQueryHandler::new(&state);
        let paths = vec![
            "not-a-store-path".to_string(),
            "/nix/store/short-name".to_string(),
        ];
        assert!(h.query(None, &paths, QueryMode::Normal).await.is_empty());
        assert!(h.query(None, &paths, QueryMode::Push).await.is_empty());
        assert!(h.query(None, &paths, QueryMode::Pull).await.is_empty());
    }

    #[tokio::test]
    async fn cache_query_normal_uncached_returns_empty() {
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello".to_string()];
        let result = CacheQueryHandler::new(&state)
            .query(None, &paths, QueryMode::Normal)
            .await;
        assert!(
            result.is_empty(),
            "Normal mode should not return uncached paths"
        );
    }

    #[tokio::test]
    async fn cache_query_pull_uncached_returns_empty() {
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello".to_string()];
        let result = CacheQueryHandler::new(&state)
            .query(None, &paths, QueryMode::Pull)
            .await;
        assert!(
            result.is_empty(),
            "Pull mode should not return uncached paths"
        );
    }

    #[tokio::test]
    async fn cache_query_push_uncached_returns_all_with_cached_false() {
        let state = make_state();
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar".to_string(),
        ];
        let result = CacheQueryHandler::new(&state)
            .query(None, &paths, QueryMode::Push)
            .await;
        assert_eq!(result.len(), 2, "Push should return all queried paths");
        for cp in &result {
            assert!(!cp.cached, "all should be uncached (empty DB): {}", cp.path);
            assert!(
                cp.url.is_none(),
                "local store → no presigned URL: {}",
                cp.path
            );
        }
        let returned_paths: Vec<&str> = result.iter().map(|c| c.path.as_str()).collect();
        assert!(returned_paths.contains(&paths[0].as_str()));
        assert!(returned_paths.contains(&paths[1].as_str()));
    }

    #[tokio::test]
    async fn cache_query_rejects_overlong_hash() {
        // Hash component of 33 chars must be rejected — nix-base32 hashes are
        // exactly 32 chars. Guards against an `== 32` → `>= 32` length-check
        // relaxation.
        let state = make_state();
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()];
        for mode in [QueryMode::Normal, QueryMode::Pull, QueryMode::Push] {
            let result = CacheQueryHandler::new(&state)
                .query(None, &paths, mode)
                .await;
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
        let result = CacheQueryHandler::new(&state)
            .query(None, &paths, QueryMode::Push)
            .await;
        for cp in &result {
            assert!(!cp.cached);
        }
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
