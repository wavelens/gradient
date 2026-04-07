/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use chrono::Utc;
use core::sources::{
    clear_key, format_cache_key, get_cache_nar_compressed_location, get_cache_nar_location,
    get_hash_from_path, get_path_from_build_output, write_key,
};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, Statement,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Symlink name used for the GC root pinning a given build output.
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
        let builds = get_next_build_outputs(Arc::clone(&state), concurrency).await;

        if builds.is_empty() {
            interval.tick().await;

            // Periodically run cleanup
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
                if state.cli.nar_ttl_hours > 0 {
                    if let Err(e) = cleanup_stale_cached_nars(Arc::clone(&state)).await {
                        error!(error = %e, "NAR TTL GC failed");
                    } else {
                        info!("NAR TTL GC completed successfully");
                    }
                }
            }
        } else {
            let tasks: Vec<_> = builds
                .into_iter()
                .map(|build| {
                    let s = Arc::clone(&state);
                    tokio::spawn(async move { cache_build_output(s, build).await })
                })
                .collect();
            for task in tasks {
                let _ = task.await;
            }
        }
    }
}

pub async fn cache_build_output(state: Arc<ServerState>, build_output: MBuildOutput) {
    let build = match EBuild::find_by_id(build_output.build).one(&state.db).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            error!("Build not found: {}", build_output.build);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query build");
            return;
        }
    };

    let evaluation = match EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await
    {
        Ok(Some(e)) => e,
        Ok(None) => {
            error!("Evaluation not found: {}", build.evaluation);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query evaluation");
            return;
        }
    };

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = match EProject::find_by_id(project_id).one(&state.db).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                error!("Project not found: {}", project_id);
                return;
            }
            Err(e) => {
                error!(error = %e, "Failed to query project");
                return;
            }
        };
        project.organization
    } else {
        match EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
        {
            Ok(Some(d)) => d.organization,
            Ok(None) => {
                error!("Direct build not found for evaluation: {}", evaluation.id);
                return;
            }
            Err(e) => {
                error!(error = %e, "Failed to query direct build");
                return;
            }
        }
    };

    let organization = match EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
    {
        Ok(Some(o)) => o,
        Ok(None) => {
            error!("Organization not found: {}", organization_id);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query organization");
            return;
        }
    };

    let path = get_path_from_build_output(build_output.clone());

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

    for cache in active_caches {
        sign_build_output(Arc::clone(&state), cache, build_output.clone()).await;
    }

    let is_entry_point = EEntryPoint::find()
        .filter(CEntryPoint::Build.eq(build.id))
        .one(&state.db)
        .await
        .unwrap_or(None)
        .is_some();

    info!(
        hash = %build_output.hash,
        package = %build_output.package,
        is_entry_point,
        "Caching build output"
    );

    let pack_result =
        pack_build_output(Arc::clone(&state), build_output.clone(), is_entry_point).await;

    let (file_hash, file_size) = match pack_result {
        Ok(result) => result,
        Err(e) => {
            error!(error = %e, "Failed to pack build output");
            return;
        }
    };

    let mut abuild_output = build_output.clone().into_active_model();

    abuild_output.file_hash = Set(Some(file_hash));
    abuild_output.file_size = Set(Some(file_size as i64));
    abuild_output.is_cached = Set(true);

    if let Err(e) = abuild_output.update(&state.db).await {
        error!(error = %e, "Failed to update build output cache status");
        return;
    }

    create_gcroot(&state, &build_output.hash, &build_output.package).await;
}

pub async fn sign_build_output(state: Arc<ServerState>, cache: MCache, build_output: MBuildOutput) {
    let path = get_path_from_build_output(build_output.clone());
    let secret_key = match format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
    ) {
        Ok(key) => {
            debug!("Found secret key for cache '{}'", cache.name);
            key
        }
        Err(e) => {
            error!("Failed to format cache key: {}", e);
            return;
        }
    };

    let key_file = match write_key(secret_key.clone()) {
        Ok(file) => file,
        Err(e) => {
            error!(error = %e, "Failed to write cache key file");
            return;
        }
    };

    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("store")
        .arg("sign")
        .arg("-k")
        .arg(key_file.clone())
        .arg(path.clone())
        .output()
        .await
        .map_err(|e| e.to_string())
    {
        Ok(output) => output,
        Err(e) => {
            error!(error = %e, "Error while executing nix store sign command");
            return;
        }
    };

    if !output.status.success() {
        error!(
            "Could not sign path with nix store sign. Exit code: {:?}, stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    debug!("Successfully signed path: {}", path);

    if let Err(e) = clear_key(key_file) {
        error!(error = %e, "Failed to clear cache key file");
    }

    let nix_cmd = ["path-info", "--sigs", &path];
    debug!(
        "Running command: {} {}",
        state.cli.binpath_nix,
        nix_cmd.join(" ")
    );

    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("path-info")
        .arg("--sigs")
        .arg(path.clone())
        .output()
        .await
        .map_err(|e| e.to_string())
    {
        Ok(output) => output,
        Err(e) => {
            error!(error = %e, "Error while executing nix path-info --sigs command");
            return;
        }
    };

    if !output.status.success() {
        error!(
            "Could not get path info with nix path-info. Exit code: {:?}, stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let signatures = String::from_utf8_lossy(&output.stdout).to_string();
    debug!(
        "Signature output for cache '{}': {}",
        cache.name, signatures
    );

    let cache_identifier = secret_key.split(':').next().unwrap_or(&cache.name);
    debug!(
        "Looking for cache identifier '{}' in signatures",
        cache_identifier
    );

    let mut signature = String::new();
    for mut line in signatures.split(" ") {
        line = line.trim();
        debug!("Checking signature line: '{}'", line);
        if let Some(sig_part) = line.split_whitespace().last() {
            debug!("Found signature part: '{}'", sig_part);
            if sig_part.starts_with(&format!("{}:", cache_identifier)) {
                if let Some(actual_sig) = sig_part.split(':').nth(1) {
                    signature = actual_sig.trim().to_string();
                    debug!("Extracted signature: {}", signature);
                    break;
                }
            } else {
                debug!(
                    "Signature part doesn't start with '{}:': {}",
                    cache_identifier, sig_part
                );
            }
        }
    }

    if signature.is_empty() {
        error!(
            "No signature found for cache '{}' in output. Lines checked:",
            cache.name
        );
        for (i, line) in signatures.split(" ").enumerate() {
            error!("  Line {}: {}", i + 1, line.trim());
        }
        return;
    }

    let build_path_signature = ABuildOutputSignature {
        id: Set(Uuid::new_v4()),
        build_output: Set(build_output.id),
        cache: Set(cache.id),
        signature: Set(signature),
        created_at: Set(Utc::now().naive_utc()),
    };

    if let Err(e) = build_path_signature.insert(&state.db).await {
        error!(error = %e, "Failed to insert build output signature");
    } else {
        debug!(
            "Successfully inserted signature for build output {}",
            build_output.id
        );
    }
}

pub async fn pack_build_output(
    state: Arc<ServerState>,
    build_output: MBuildOutput,
    is_entry_point: bool,
) -> Result<(String, u32)> {
    let path = get_path_from_build_output(build_output);

    let (path_hash, _path_package) =
        get_hash_from_path(path.clone()).context("Failed to parse build output path")?;

    // Pack the NAR into memory
    let pack_output = Command::new(state.cli.binpath_nix.clone())
        .arg("nar")
        .arg("pack")
        .arg(&path)
        .output()
        .await
        .context("Failed to execute nix nar pack command")?;

    if !pack_output.status.success() {
        bail!("Nix nar pack command failed");
    }

    let nar_data = pack_output.stdout;
    // Keep a copy for on-disk persistence (entry-point builds only).
    let nar_data_for_disk = if is_entry_point {
        nar_data.clone()
    } else {
        vec![]
    };

    // Compress in memory to compute file_hash / file_size — no disk writes.
    // Must use the same level (3) as the web handler uses when serving, so that
    // the narinfo FileHash matches the bytes clients actually receive.
    let compressed_data = tokio::task::spawn_blocking(move || zstd::bulk::compress(&nar_data, 6))
        .await
        .context("Compression task panicked")?
        .context("Failed to compress NAR data")?;

    let file_size = compressed_data.len() as u32;
    let file_hash = nix_base32_sha256(&compressed_data);

    // Only persist the raw NAR to disk for entry-point builds;
    // non-entry-point NARs are compressed on the fly when served
    if is_entry_point {
        let nar_location = get_cache_nar_location(state.cli.base_path.clone(), path_hash)
            .context("Failed to get NAR file location")?;

        tokio::fs::write(&nar_location, &nar_data_for_disk)
            .await
            .context("Failed to write entry-point NAR to disk")?;
    }

    Ok((format!("sha256:{}", file_hash), file_size))
}

/// Compute SHA-256 of `data` and return it encoded in Nix's base-32 alphabet.
///
/// Nix base-32 uses the alphabet `0123456789abcdfghijklmnpqrsvwxyz` (no e/o/t/u)
/// and encodes 5 bits per character, most-significant group first.
fn nix_base32_sha256(data: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let hash: [u8; 32] = Sha256::digest(data).into();
    let len = (hash.len() * 8 - 1) / 5 + 1; // 52 for SHA-256
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

async fn get_next_build_outputs(state: Arc<ServerState>, limit: usize) -> Vec<MBuildOutput> {
    EBuildOutput::find()
        .filter(CBuildOutput::IsCached.eq(false))
        .order_by_asc(CBuildOutput::CreatedAt)
        .limit(limit as u64)
        .all(&state.db)
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "Failed to query next build outputs");
            vec![]
        })
}

pub async fn invalidate_cache_for_path(state: Arc<ServerState>, path: String) -> Result<()> {
    let (hash, package) = get_hash_from_path(path.clone())
        .with_context(|| format!("Failed to parse path {}", path))?;

    let build_outputs = EBuildOutput::find()
        .filter(
            Condition::all()
                .add(CBuildOutput::Hash.eq(hash.clone()))
                .add(CBuildOutput::Package.eq(package.clone()))
                .add(CBuildOutput::IsCached.eq(true)),
        )
        .all(&state.db)
        .await
        .context("Database error while finding build outputs")?;

    for build_output in build_outputs {
        let mut abuild_output = build_output.clone().into_active_model();
        abuild_output.is_cached = Set(false);
        abuild_output.file_hash = Set(None);
        abuild_output.file_size = Set(None);
        abuild_output
            .update(&state.db)
            .await
            .context("Failed to update build output")?;

        let file_location = get_cache_nar_location(state.cli.base_path.clone(), hash.clone())
            .context("Failed to get cache NAR location")?;
        if std::fs::metadata(&file_location).is_ok() {
            std::fs::remove_file(&file_location)
                .with_context(|| format!("Failed to remove cached file {}", file_location))?;
        }

        let signatures = EBuildOutputSignature::find()
            .filter(CBuildOutputSignature::BuildOutput.eq(build_output.id))
            .all(&state.db)
            .await
            .context("Failed to find build output signatures")?;

        for signature in signatures {
            let asignature = signature.into_active_model();
            asignature
                .delete(&state.db)
                .await
                .context("Failed to delete signature")?;
        }

        info!(path = %path, "Invalidated cache for path");
    }

    Ok(())
}

/// Runs per-project evaluation GC for all projects that have `keep_evaluations > 0`.
///
/// Uses each project's own `keep_evaluations` as the retention limit.  The global
/// `GRADIENT_KEEP_EVALUATIONS` CLI value acts only as a cap enforced at write time
/// (see `patch_project`); this function does not re-enforce it.
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

/// Garbage-collects non-entry-point cached NARs whose `last_fetched_at` is older than
/// `GRADIENT_NAR_TTL_HOURS`. For each stale output:
///   1. Deletes the on-disk compressed NAR (`.nar.zst`).
///   2. Attempts `nix store delete` to free nix store space (skipped silently if the path
///      is still reachable from a GC root, e.g. as a dependency of an entry-point).
///   3. Marks `is_cached = false` only when the nix store deletion succeeds.
pub async fn cleanup_stale_cached_nars(state: Arc<ServerState>) -> Result<()> {
    let ttl_hours = state.cli.nar_ttl_hours;
    if ttl_hours == 0 {
        return Ok(());
    }

    // Find non-entry-point outputs that haven't been fetched within the TTL.
    let rows = state
        .db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT bo.id, bo.hash, bo.package
               FROM build_output bo
               JOIN build b ON b.id = bo.build
               WHERE bo.is_cached = true
                 AND bo.last_fetched_at IS NOT NULL
                 AND bo.last_fetched_at < NOW() AT TIME ZONE 'UTC' - ($1 * INTERVAL '1 hour')
                 AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.build = b.id)"#,
            [sea_orm::Value::BigInt(Some(ttl_hours as i64))],
        ))
        .await
        .context("Failed to query stale cached NARs")?;

    for row in rows {
        let id: uuid::Uuid = match row.try_get("", "id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let hash: String = match row.try_get("", "hash") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let package: String = match row.try_get("", "package") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let store_path = format!("/nix/store/{}-{}", hash, package);

        // 1. Delete the compressed NAR cache file (best-effort).
        if let Ok(zst_path) =
            get_cache_nar_compressed_location(state.cli.base_path.clone(), hash.clone())
            && let Err(e) = tokio::fs::remove_file(&zst_path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!(error = %e, path = %zst_path, "Failed to remove stale compressed NAR");
        }

        // 2. Remove the GC root so the path is no longer pinned by us.
        remove_gcroot(&state, &hash, &package).await;

        // 3. Try to delete from the nix store. This will fail if the path is still
        //    reachable from another GC root (e.g. dependency of a live entry-point),
        //    which is intentional — we only free what is truly unreachable.
        let store_deleted = match state.nix_store.delete_path(store_path.clone()).await {
            Ok(true) => {
                info!(store_path = %store_path, "GC'd stale NAR from nix store");
                true
            }
            Ok(false) => {
                debug!(
                    store_path = %store_path,
                    "nix store delete skipped (path still referenced)"
                );
                false
            }
            Err(e) => {
                warn!(error = %e, store_path = %store_path, "Failed to delete from nix store");
                false
            }
        };

        // 4. Mark is_cached = false only if the store path was actually deleted.
        if store_deleted && let Ok(Some(bo)) = EBuildOutput::find_by_id(id).one(&state.db).await {
            let mut active = bo.into_active_model();
            active.is_cached = Set(false);
            active.file_hash = Set(None);
            active.file_size = Set(None);
            active.last_fetched_at = Set(None);
            if let Err(e) = active.update(&state.db).await {
                warn!(error = %e, id = %id, "Failed to mark build_output as uncached after GC");
            }
        }
    }

    Ok(())
}

pub async fn cleanup_orphaned_cache_files(state: Arc<ServerState>) -> Result<()> {
    let cache_dir = format!("{}/nars", state.cli.base_path);

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

                if file_path.extension().and_then(|s| s.to_str()) == Some("nar")
                    && let Some(hash_part) = file_path.file_stem().and_then(|s| s.to_str())
                {
                    let parent_dir = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    let full_hash = format!("{}{}", parent_dir, hash_part);

                    let build_output_exists = EBuildOutput::find()
                        .filter(
                            Condition::all()
                                .add(CBuildOutput::Hash.eq(full_hash.clone()))
                                .add(CBuildOutput::IsCached.eq(true)),
                        )
                        .one(&state.db)
                        .await
                        .context("Failed to check if build output exists")?
                        .is_some();

                    if !build_output_exists {
                        orphaned_files.push(file_path);
                    }
                }
            }
        }
    }

    // Remove orphaned files
    for file_path in orphaned_files {
        if let Err(e) = std::fs::remove_file(&file_path) {
            error!(file_path = ?file_path, error = %e, "Failed to remove orphaned cache file");
        } else {
            debug!(file_path = ?file_path, "Removed orphaned cache file");
        }
    }

    Ok(())
}
