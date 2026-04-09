/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::Utc;
use core::sources::{format_cache_key, get_hash_from_path, get_path_from_derivation_output};
use core::types::*;
use harmonia_store_core::signature::{SecretKey, fingerprint_path};
use harmonia_store_core::store_path::{StoreDir, StorePath};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, Statement,
};
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Symlink name used for the GC root pinning a given derivation output.
fn gcroot_name(hash: &str, package: &str) -> String {
    format!("{}-{}", hash, package)
}

async fn create_gcroot(state: &Arc<ServerState>, hash: &str, package: &str) {
    let store_path = format!("/nix/store/{}-{}", hash, package);
    let name = gcroot_name(hash, package);
    if let Err(e) = state.nix_store.add_gcroot(name.clone(), store_path).await {
        warn!(error = %e, name = %name, "Failed to create GC root");
    } else {
        debug!(name = %name, "Created GC root");
    }
}

async fn remove_gcroot(state: &Arc<ServerState>, hash: &str, package: &str) {
    let name = gcroot_name(hash, package);
    if let Err(e) = state.nix_store.remove_gcroot(name.clone()).await {
        warn!(error = %e, name = %name, "Failed to remove GC root");
    } else {
        debug!(name = %name, "Removed GC root");
    }
}

pub async fn cache_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let concurrency = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut interval = time::interval(Duration::from_secs(5));
    let mut cleanup_counter = 0;
    const CLEANUP_INTERVAL: u32 = 720;

    loop {
        let outputs = get_next_uncached_derivation_outputs(Arc::clone(&state), concurrency).await;

        if outputs.is_empty() {
            interval.tick().await;

            cleanup_counter += 1;
            if cleanup_counter >= CLEANUP_INTERVAL {
                cleanup_counter = 0;
                if let Err(e) = cleanup_orphaned_cache_files(Arc::clone(&state)).await {
                    error!(error = %e, "Cache cleanup failed");
                } else {
                    info!("Cache cleanup completed successfully");
                }
                if let Err(e) = cleanup_old_evaluations(Arc::clone(&state)).await {
                    error!(error = %e, "Evaluation GC failed");
                } else {
                    info!("Evaluation GC completed successfully");
                }
                if let Err(e) = core::gc::gc_orphan_derivations(
                    Arc::clone(&state),
                    state.cli.keep_orphan_derivations_hours,
                )
                .await
                {
                    error!(error = %e, "Derivation GC failed");
                } else {
                    info!("Derivation GC completed successfully");
                }
                if state.cli.nar_ttl_hours > 0
                    && let Err(e) = cleanup_stale_cached_nars(Arc::clone(&state)).await
                {
                    error!(error = %e, "NAR TTL GC failed");
                }
            }
        } else {
            let tasks: Vec<_> = outputs
                .into_iter()
                .map(|output| {
                    let s = Arc::clone(&state);
                    tokio::spawn(async move { cache_derivation_output(s, output).await })
                })
                .collect();
            for task in tasks {
                let _ = task.await;
            }
        }
    }
}

/// Caches a single derivation output to all caches subscribed by its owning organisation.
///
/// After all outputs of a derivation are `is_cached = true` and the closure is fully
/// cached, the caller (driven from `cache_loop`) records a `cache_derivation` row.
pub async fn cache_derivation_output(state: Arc<ServerState>, output: MDerivationOutput) {
    let derivation = match EDerivation::find_by_id(output.derivation)
        .one(&state.db)
        .await
    {
        Ok(Some(d)) => d,
        Ok(None) => {
            error!("Derivation not found: {}", output.derivation);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query derivation");
            return;
        }
    };

    let organization = match EOrganization::find_by_id(derivation.organization)
        .one(&state.db)
        .await
    {
        Ok(Some(o)) => o,
        Ok(None) => {
            error!("Organization not found: {}", derivation.organization);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query organization");
            return;
        }
    };

    let path = get_path_from_derivation_output(output.clone());

    match state.nix_store.query_pathinfo(path.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            warn!(path = %path, "Path not found in local store, skipping cache");
            return;
        }
        Err(e) => {
            error!(error = %e, path = %path, "Failed to query local store, skipping cache");
            return;
        }
    }

    let cache_ids: Vec<Uuid> = match EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(organization.id))
        .all(&state.db)
        .await
    {
        Ok(ocs) => ocs.into_iter().map(|oc| oc.cache).collect(),
        Err(e) => {
            error!(error = %e, "Failed to query organization caches");
            return;
        }
    };

    let active_caches = match ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .filter(CCache::Active.eq(true))
        .all(&state.db)
        .await
    {
        Ok(caches) => caches,
        Err(e) => {
            error!(error = %e, "Failed to query active caches");
            return;
        }
    };

    for cache in &active_caches {
        sign_derivation_output(Arc::clone(&state), cache.clone(), output.clone()).await;
    }

    info!(
        hash = %output.hash,
        package = %output.package,
        "Caching derivation output"
    );

    let pack_result = pack_derivation_output(Arc::clone(&state), output.clone()).await;

    let (file_hash, file_size, nar_size) = match pack_result {
        Ok(result) => result,
        Err(e) => {
            error!(error = %e, hash = %output.hash, "Failed to pack derivation output: {:#}", e);
            return;
        }
    };

    let mut active = output.clone().into_active_model();
    active.file_hash = Set(Some(file_hash));
    active.file_size = Set(Some(file_size as i64));
    active.nar_size = Set(Some(nar_size as i64));
    info!(
        hash = %output.hash,
        file_size = file_size,
        nar_size = nar_size,
        "Packed and uploaded NAR"
    );
    active.is_cached = Set(true);

    if let Err(e) = active.update(&state.db).await {
        error!(error = %e, "Failed to update derivation output cache status");
        return;
    }

    create_gcroot(&state, &output.hash, &output.package).await;

    // After updating, check whether this derivation's full closure is now
    // available in any of the caches. If so, record the cache_derivation row.
    for cache in &active_caches {
        if let Err(e) =
            try_record_cache_derivation(Arc::clone(&state), cache.id, derivation.id).await
        {
            warn!(error = %e, cache_id = %cache.id, drv_id = %derivation.id,
                "Failed to record cache_derivation");
        }
    }
}

/// If every output of `derivation_id` is cached AND every transitive dependency already
/// has a `cache_derivation` row for `cache_id`, insert the row for this derivation.
async fn try_record_cache_derivation(
    state: Arc<ServerState>,
    cache_id: Uuid,
    derivation_id: Uuid,
) -> Result<()> {
    // 1. All outputs of this derivation cached?
    let any_uncached = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation_id))
        .filter(CDerivationOutput::IsCached.eq(false))
        .one(&state.db)
        .await?
        .is_some();
    if any_uncached {
        return Ok(());
    }

    // 2. Every direct dependency has a cache_derivation row for this cache.
    let dep_edges = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(derivation_id))
        .all(&state.db)
        .await?;
    for edge in dep_edges {
        let present = ECacheDerivation::find()
            .filter(CCacheDerivation::Cache.eq(cache_id))
            .filter(CCacheDerivation::Derivation.eq(edge.dependency))
            .one(&state.db)
            .await?
            .is_some();
        if !present {
            return Ok(());
        }
    }

    // 3. Already recorded?
    let already = ECacheDerivation::find()
        .filter(CCacheDerivation::Cache.eq(cache_id))
        .filter(CCacheDerivation::Derivation.eq(derivation_id))
        .one(&state.db)
        .await?
        .is_some();
    if already {
        return Ok(());
    }

    let row = ACacheDerivation {
        id: Set(Uuid::new_v4()),
        cache: Set(cache_id),
        derivation: Set(derivation_id),
        cached_at: Set(Utc::now().naive_utc()),
        last_fetched_at: Set(None),
    };
    row.insert(&state.db).await?;
    debug!(cache_id = %cache_id, derivation_id = %derivation_id, "Recorded cache_derivation");
    Ok(())
}

pub async fn sign_derivation_output(
    state: Arc<ServerState>,
    cache: MCache,
    output: MDerivationOutput,
) {
    let path = get_path_from_derivation_output(output.clone());
    let secret_key_str = match format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
    ) {
        Ok(key) => key,
        Err(e) => {
            error!("Failed to format cache key: {}", e);
            return;
        }
    };

    let secret_key: SecretKey = match secret_key_str.parse() {
        Ok(k) => k,
        Err(e) => {
            error!(error = %e, "Failed to parse secret key");
            return;
        }
    };

    let pathinfo = match state.nix_store.query_pathinfo(path.clone()).await {
        Ok(Some(info)) => info,
        Ok(None) => {
            error!(path = %path, "Path not found in store, cannot sign");
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query path info for signing");
            return;
        }
    };

    // Convert SRI hash (sha256-<base64>) to nix format (sha256:<nix-base32>) for fingerprinting.
    let nar_hash_nix = match sri_to_nix_hash(&pathinfo.nar_hash) {
        Ok(h) => h,
        Err(e) => {
            error!(error = %e, "Failed to convert NAR hash format");
            return;
        }
    };

    let store_dir = StoreDir::default();
    let base = path
        .strip_prefix("/nix/store/")
        .unwrap_or(&path);
    let store_path = match StorePath::from_base_path(base) {
        Ok(sp) => sp,
        Err(e) => {
            error!(error = %e, path = %path, "Invalid store path for signing");
            return;
        }
    };

    let references: BTreeSet<StorePath> = pathinfo
        .references
        .iter()
        .filter_map(|r| {
            let base = r.strip_prefix("/nix/store/").unwrap_or(r);
            StorePath::from_base_path(base).ok()
        })
        .collect();

    let fingerprint = match fingerprint_path(
        &store_dir,
        &store_path,
        nar_hash_nix.as_bytes(),
        pathinfo.nar_size,
        &references,
    ) {
        Ok(fp) => fp,
        Err(e) => {
            error!(error = %e, "Failed to compute fingerprint for signing");
            return;
        }
    };

    let sig = secret_key.sign(&fingerprint);
    let sig_str = sig.to_string();

    // Register the signature in the Nix daemon's DB.
    if let Err(e) = state
        .nix_store
        .add_signatures(path.clone(), vec![sig])
        .await
    {
        warn!(error = %e, "Failed to add signature to store (non-fatal)");
    }

    // Extract the base64 part after "name:" for DB storage.
    let signature = sig_str
        .find(':')
        .map(|i| sig_str[i + 1..].to_string())
        .unwrap_or(sig_str);

    let row = ADerivationOutputSignature {
        id: Set(Uuid::new_v4()),
        derivation_output: Set(output.id),
        cache: Set(cache.id),
        signature: Set(signature),
        created_at: Set(Utc::now().naive_utc()),
    };

    if let Err(e) = row.insert(&state.db).await {
        error!(error = %e, "Failed to insert derivation output signature");
    }
}

/// Converts an SRI-format NAR hash (`sha256-<base64>`) to the Nix format
/// (`sha256:<nix-base32>`) required by `fingerprint_path`.
fn sri_to_nix_hash(sri: &str) -> Result<String> {
    use base64::Engine as _;
    let b64 = sri
        .strip_prefix("sha256-")
        .ok_or_else(|| anyhow::anyhow!("Not a sha256 SRI hash: {}", sri))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("Invalid base64 in SRI hash")?;
    Ok(format!("sha256:{}", nix_base32_encode(&raw)))
}

/// Streams NAR encoding → zstd compression → SHA-256 hash → multipart
/// upload to the NAR store.  Memory usage stays bounded regardless of NAR
/// size (one multipart part in flight at a time).
///
/// Uses `harmonia-nar`'s `NarByteStream` for pure-Rust NAR packing instead
/// of shelling out to `nix nar pack`.
pub async fn pack_derivation_output(
    state: Arc<ServerState>,
    output: MDerivationOutput,
) -> Result<(String, u64, u64)> {
    use std::io::Write as _;
    use futures::StreamExt;

    let path = get_path_from_derivation_output(output);
    let (path_hash, _) =
        get_hash_from_path(path.clone()).context("Failed to parse derivation output path")?;

    let mut nar_stream = harmonia_nar::NarByteStream::new(path.clone().into());

    // 10 MiB parts — above S3's 5 MiB minimum, large enough to reduce
    // round-trips, small enough to keep memory bounded.
    const PART_SIZE: usize = 10 * 1024 * 1024;
    let mut writer = state.nar_storage.put_streaming(&path_hash, PART_SIZE).await?;

    // Streaming zstd encoder writing compressed output into a reusable Vec.
    let mut encoder = zstd::stream::Encoder::new(Vec::with_capacity(256 * 1024), 6)
        .context("Failed to create zstd encoder")?;
    let mut hash_ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut nar_size: u64 = 0;
    let mut file_size: u64 = 0;

    let upload_result: Result<()> = async {
        while let Some(chunk_result) = nar_stream.next().await {
            let chunk = chunk_result.context("NAR stream error")?;
            nar_size += chunk.len() as u64;

            // Feed uncompressed data into the encoder; compressed output
            // accumulates in the encoder's inner Vec<u8>.
            encoder
                .write_all(&chunk)
                .context("zstd compression failed")?;

            // Drain any compressed output produced so far.
            let compressed = encoder.get_mut();
            if !compressed.is_empty() {
                hash_ctx.update(compressed);
                file_size += compressed.len() as u64;
                writer.write(compressed);
                compressed.clear();
                writer
                    .wait_for_capacity(PART_SIZE)
                    .await
                    .context("Multipart upload failed during write")?;
            }
        }

        // Flush the encoder's internal state and collect any remaining bytes.
        let remaining = encoder.finish().context("Failed to finish zstd encoder")?;
        if !remaining.is_empty() {
            hash_ctx.update(&remaining);
            file_size += remaining.len() as u64;
            writer.write(&remaining);
        }

        // Complete the multipart upload.
        writer
            .finish()
            .await
            .context("Failed to complete multipart upload")?;

        Ok(())
    }
    .await;

    // If the upload failed, the WriteMultipart was dropped which aborts it.
    upload_result?;

    let digest = hash_ctx.finish();
    let file_hash = nix_base32_encode(digest.as_ref());

    Ok((format!("sha256:{}", file_hash), file_size, nar_size))
}

/// Encode a raw hash digest in Nix's base-32 alphabet.
fn nix_base32_encode(hash: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let len = (hash.len() * 8 - 1) / 5 + 1;
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let byte0 = hash.get(i).copied().unwrap_or(0) as u32;
        let byte1 = hash.get(i + 1).copied().unwrap_or(0) as u32;
        let c = ((byte0 >> j) | (byte1 << (8 - j))) & 0x1f;
        out.push(CHARS[c as usize] as char);
    }
    out
}

/// Compute SHA-256 of `data` and return it encoded in Nix's base-32 alphabet.
///
/// Uses `ring`'s SHA-256, which dispatches at runtime to the fastest
/// implementation available on the host CPU (SHA-NI on modern x86,
/// ARMv8 crypto extensions on aarch64, AVX2 on older x86, scalar
/// fallback otherwise).
#[allow(dead_code)]
fn nix_base32_sha256(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    nix_base32_encode(digest.as_ref())
}

async fn get_next_uncached_derivation_outputs(
    state: Arc<ServerState>,
    limit: usize,
) -> Vec<MDerivationOutput> {
    EDerivationOutput::find()
        .filter(CDerivationOutput::IsCached.eq(false))
        .order_by_asc(CDerivationOutput::CreatedAt)
        .limit(limit as u64)
        .all(&state.db)
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "Failed to query next derivation outputs");
            vec![]
        })
}

/// Invalidates a path's cached state across all caches:
///   - removes its NAR file from storage
///   - clears `is_cached` / `file_hash` / `file_size` on all matching outputs
///   - deletes any `cache_derivation` rows for the owning derivation
///   - walks reverse dependency edges and deletes `cache_derivation` rows for
///     every transitive dependent in the same cache (their closures are now
///     incomplete). Their NAR files stay; only the closure assertion is revoked.
pub async fn invalidate_cache_for_path(state: Arc<ServerState>, path: String) -> Result<()> {
    let (hash, package) = get_hash_from_path(path.clone())
        .with_context(|| format!("Failed to parse path {}", path))?;

    let outputs = EDerivationOutput::find()
        .filter(
            Condition::all()
                .add(CDerivationOutput::Hash.eq(hash.clone()))
                .add(CDerivationOutput::Package.eq(package.clone()))
                .add(CDerivationOutput::IsCached.eq(true)),
        )
        .all(&state.db)
        .await
        .context("Database error while finding derivation outputs")?;

    for output in outputs {
        let derivation_id = output.derivation;

        let mut active = output.clone().into_active_model();
        active.is_cached = Set(false);
        active.file_hash = Set(None);
        active.file_size = Set(None);
        active
            .update(&state.db)
            .await
            .context("Failed to update derivation output")?;

        state
            .nar_storage
            .delete(&hash)
            .await
            .with_context(|| format!("Failed to remove cached NAR for {}", hash))?;

        let signatures = EDerivationOutputSignature::find()
            .filter(CDerivationOutputSignature::DerivationOutput.eq(output.id))
            .all(&state.db)
            .await
            .context("Failed to find derivation output signatures")?;

        for signature in signatures {
            let asignature = signature.into_active_model();
            asignature
                .delete(&state.db)
                .await
                .context("Failed to delete signature")?;
        }

        // Drop cache_derivation rows for this derivation in every cache,
        // plus walk reverse derivation_dependency edges and remove rows for
        // every dependent (its closure is no longer complete).
        revoke_cache_derivation_closure(&state, derivation_id).await?;

        info!(path = %path, "Invalidated cache for path");
    }

    Ok(())
}

/// Walks reverse `derivation_dependency` edges starting at `derivation_id` and removes
/// all `cache_derivation` rows touching the visited derivations across every cache.
async fn revoke_cache_derivation_closure(
    state: &Arc<ServerState>,
    derivation_id: Uuid,
) -> Result<()> {
    let mut visited: HashSet<Uuid> = HashSet::new();
    let mut frontier = vec![derivation_id];
    visited.insert(derivation_id);

    while !frontier.is_empty() {
        let edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Dependency.is_in(frontier.clone()))
            .all(&state.db)
            .await
            .context("Failed to walk reverse derivation_dependency")?;
        frontier.clear();
        for edge in edges {
            if visited.insert(edge.derivation) {
                frontier.push(edge.derivation);
            }
        }
    }

    let drv_ids: Vec<Uuid> = visited.into_iter().collect();
    let cache_rows = ECacheDerivation::find()
        .filter(CCacheDerivation::Derivation.is_in(drv_ids))
        .all(&state.db)
        .await
        .context("Failed to query cache_derivation rows")?;

    for row in cache_rows {
        let active = row.into_active_model();
        if let Err(e) = active.delete(&state.db).await {
            warn!(error = %e, "Failed to delete cache_derivation row");
        }
    }

    Ok(())
}

pub async fn cleanup_old_evaluations(state: Arc<ServerState>) -> Result<()> {
    let projects = EProject::find()
        .all(&state.db)
        .await
        .context("Failed to query projects for evaluation GC")?;

    for project in projects {
        let keep = project.keep_evaluations as usize;
        if keep == 0 {
            continue;
        }
        if let Err(e) = core::gc::gc_project_evaluations(Arc::clone(&state), project.id, keep).await
        {
            warn!(error = %e, project_id = %project.id, "Evaluation GC failed for project");
        }
    }

    Ok(())
}

/// Cache NAR TTL pass: deletes `cache_derivation` rows whose `last_fetched_at` is older
/// than `nar_ttl_hours`. For each expired row, deletes the NAR file from storage and
/// drops the row. The derivation and its outputs stay (other caches may still hold them).
pub async fn cleanup_stale_cached_nars(state: Arc<ServerState>) -> Result<()> {
    let ttl_hours = state.cli.nar_ttl_hours;
    if ttl_hours == 0 {
        return Ok(());
    }

    let rows = state
        .db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT cd.id, cd.cache, cd.derivation
               FROM cache_derivation cd
               WHERE cd.last_fetched_at IS NOT NULL
                 AND cd.last_fetched_at < NOW() AT TIME ZONE 'UTC' - ($1 * INTERVAL '1 hour')"#,
            [sea_orm::Value::BigInt(Some(ttl_hours as i64))],
        ))
        .await
        .context("Failed to query stale cache_derivation rows")?;

    for row in rows {
        let cd_id: Uuid = match row.try_get("", "id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let drv_id: Uuid = match row.try_get("", "derivation") {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Find the outputs of the derivation; remove their NAR files (if no other
        // cache_derivation row keeps them alive).
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(drv_id))
            .all(&state.db)
            .await
            .unwrap_or_default();

        // Drop the cache_derivation row first; revocation of dependents follows.
        if let Some(cd) = ECacheDerivation::find_by_id(cd_id)
            .one(&state.db)
            .await
            .ok()
            .flatten()
        {
            let _ = cd.into_active_model().delete(&state.db).await;
        }

        // Best-effort: NAR file is shared by every cache for this output, so only
        // delete when no cache_derivation row remains for the derivation.
        let still_held = ECacheDerivation::find()
            .filter(CCacheDerivation::Derivation.eq(drv_id))
            .one(&state.db)
            .await
            .ok()
            .flatten()
            .is_some();
        if !still_held {
            for o in &outputs {
                if let Err(e) = state.nar_storage.delete(&o.hash).await {
                    warn!(error = %e, hash = %o.hash, "Failed to remove stale compressed NAR");
                }
                remove_gcroot(&state, &o.hash, &o.package).await;
            }
        }
    }

    Ok(())
}

pub async fn cleanup_orphaned_cache_files(state: Arc<ServerState>) -> Result<()> {
    let Some(base_path) = state.nar_storage.local_base() else {
        return Ok(());
    };

    let cache_dir = format!("{}/nars", base_path);

    if !std::path::Path::new(&cache_dir).exists() {
        return Ok(());
    }

    let mut orphaned_files = Vec::new();

    for entry in std::fs::read_dir(&cache_dir).context("Failed to read cache directory")? {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        if path.is_dir() {
            for subentry in std::fs::read_dir(&path).context("Failed to read subdirectory")? {
                let subentry = subentry.context("Failed to read subdirectory entry")?;
                let file_path = subentry.path();

                if file_path.extension().and_then(|s| s.to_str()) == Some("zst")
                    && let Some(stem) = file_path.file_stem().and_then(|s| s.to_str())
                    && let Some(hash_part) = stem.strip_suffix(".nar")
                {
                    let parent_dir = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    let full_hash = format!("{}{}", parent_dir, hash_part);

                    let exists = EDerivationOutput::find()
                        .filter(
                            Condition::all()
                                .add(CDerivationOutput::Hash.eq(full_hash.clone()))
                                .add(CDerivationOutput::IsCached.eq(true)),
                        )
                        .one(&state.db)
                        .await
                        .context("Failed to check if derivation output exists")?
                        .is_some();

                    if !exists {
                        orphaned_files.push(file_path);
                    }
                }
            }
        }
    }

    for file_path in orphaned_files {
        if let Err(e) = std::fs::remove_file(&file_path) {
            error!(file_path = ?file_path, error = %e, "Failed to remove orphaned cache file");
        } else {
            debug!(file_path = ?file_path, "Removed orphaned cache file");
        }
    }

    Ok(())
}
