/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use chrono::Utc;
use core::sources::{
    clear_key, format_cache_key, get_cache_nar_location, get_hash_from_path,
    get_path_from_build_output, write_key,
};
use core::types::*;
use nix_daemon::{Progress, Store};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::time;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub async fn cache_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let mut interval = time::interval(Duration::from_secs(5));
    let mut cleanup_counter = 0;
    const CLEANUP_INTERVAL: u32 = 720;

    loop {
        let build = get_next_build_output(Arc::clone(&state)).await;

        if let Some(build) = build {
            cache_build_output(Arc::clone(&state), build).await;
        } else {
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

    let local_store = core::executer::get_local_store(Some(organization.clone())).await;
    if let Ok(mut local_store) = local_store {
        let path_exists = match local_store {
            core::types::LocalNixStore::UnixStream(ref mut store) => store
                .query_pathinfo(path.clone())
                .result()
                .await
                .unwrap_or(None)
                .is_some(),
            core::types::LocalNixStore::CommandDuplex(ref mut store) => store
                .query_pathinfo(path.clone())
                .result()
                .await
                .unwrap_or(None)
                .is_some(),
        };

        if !path_exists {
            warn!(path = %path, "Path not found in local store, skipping cache");
            return;
        }
    } else {
        error!(path = %path, "Failed to connect to local store, skipping cache");
        return;
    }

    let organization_caches = match EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(organization.id))
        .all(&state.db)
        .await
    {
        Ok(caches) => caches,
        Err(e) => {
            error!(error = %e, "Failed to query organization caches");
            return;
        }
    };

    for organization_cache in organization_caches {
        let ocs = match EOrganizationCache::find()
            .filter(COrganizationCache::Cache.eq(organization_cache.cache))
            .all(&state.db)
            .await
        {
            Ok(caches) => caches,
            Err(e) => {
                error!(error = %e, "Failed to query cache organizations");
                continue;
            }
        };

        for oc in ocs {
            let cache = match ECache::find_by_id(oc.cache).one(&state.db).await {
                Ok(Some(c)) => c,
                Ok(None) => {
                    error!("Cache not found: {}", oc.cache);
                    continue;
                }
                Err(e) => {
                    error!(error = %e, "Failed to query cache");
                    continue;
                }
            };

            if cache.active {
                sign_build_output(Arc::clone(&state), cache.clone(), build_output.clone()).await;
            }
        }
    }

    info!(
        hash = %build_output.hash,
        package = %build_output.package,
        "Caching build output"
    );
    let pack_result = pack_build_output(Arc::clone(&state), build_output.clone()).await;
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
    }
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
        },
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
    debug!("Running command: {} {}", state.cli.binpath_nix, nix_cmd.join(" "));

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
    debug!("Signature output for cache '{}': {}", cache.name, signatures);

    let cache_identifier = secret_key.split(':').next().unwrap_or(&cache.name);
    debug!("Looking for cache identifier '{}' in signatures", cache_identifier);

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
                debug!("Signature part doesn't start with '{}:': {}", cache_identifier, sig_part);
            }
        }
    }

    if signature.is_empty() {
        error!("No signature found for cache '{}' in output. Lines checked:", cache.name);
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
        debug!("Successfully inserted signature for build output {}", build_output.id);
    }
}

pub async fn pack_build_output(
    state: Arc<ServerState>,
    build_output: MBuildOutput,
) -> Result<(String, u32)> {
    let path = get_path_from_build_output(build_output);

    let (path_hash, _path_package) =
        get_hash_from_path(path.clone()).context("Failed to parse build output path")?;
    let file_location_tmp =
        get_cache_nar_location(state.cli.base_path.clone(), path_hash.clone(), false)
            .context("Failed to get cache location for temp file")?;
    let file_location =
        get_cache_nar_location(state.cli.base_path.clone(), path_hash.clone(), true)
            .context("Failed to get cache location")?;

    let output = Command::new(state.cli.binpath_nix.clone())
        .arg("nar")
        .arg("pack")
        .arg(path)
        .output()
        .await
        .context("Failed to execute nix nar pack command")?;

    if !output.status.success() {
        anyhow::bail!("Nix nar pack command failed");
    }

    tokio::fs::write(file_location_tmp.clone(), output.stdout)
        .await
        .context("Failed to write temporary NAR file")?;

    let input_data = tokio::fs::read(file_location_tmp.clone())
        .await
        .context("Failed to read temporary NAR file")?;

    let compressed_data =
        zstd::bulk::compress(&input_data, 19).context("Failed to compress NAR data")?;

    tokio::fs::write(file_location.clone(), compressed_data)
        .await
        .context("Failed to write compressed NAR file")?;

    tokio::fs::remove_file(file_location_tmp.clone())
        .await
        .context("Failed to remove temporary NAR file")?;

    let output = Command::new(state.cli.binpath_nix.clone())
        .arg("hash")
        .arg("file")
        .arg("--base32")
        .arg(file_location.clone())
        .output()
        .await
        .context("Failed to execute nix hash file command")?;

    if !output.status.success() {
        bail!("Nix hash file command failed");
    }

    let file_hash = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let file_metadata = std::fs::metadata(file_location).context("Failed to get file metadata")?;
    let file_size = file_metadata.len() as u32;

    Ok((format!("sha256:{}", file_hash), file_size))
}

async fn get_next_build_output(state: Arc<ServerState>) -> Option<MBuildOutput> {
    EBuildOutput::find()
        .filter(CBuildOutput::IsCached.eq(false))
        .order_by_asc(CBuildOutput::CreatedAt)
        .one(&state.db)
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "Failed to query next build output");
            None
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

        let file_location = get_cache_nar_location(state.cli.base_path.clone(), hash.clone(), true)
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

                if file_path.extension().and_then(|s| s.to_str()) == Some("zst")
                    && let Some(file_name) = file_path.file_stem().and_then(|s| s.to_str())
                    && let Some(hash_part) = file_name.strip_suffix(".nar") {
                        let parent_dir =
                            path.file_name().and_then(|s| s.to_str()).unwrap_or("");
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
