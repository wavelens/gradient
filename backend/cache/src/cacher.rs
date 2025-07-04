/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
    const CLEANUP_INTERVAL: u32 = 720; // Run cleanup every hour (720 * 5 seconds)

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
                    eprintln!("Cache cleanup failed: {}", e);
                } else {
                    println!("Cache cleanup completed successfully");
                }
            }
        }
    }
}

pub async fn cache_build_output(state: Arc<ServerState>, build_output: MBuildOutput) {
    let build = EBuild::find_by_id(build_output.build)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let path = get_path_from_build_output(build_output.clone());

    // Check if path exists in local Nix store before caching
    let local_store = core::executer::get_local_store(Some(organization.clone())).await;
    if let Ok(mut local_store) = local_store {
        let path_exists = match local_store {
            core::types::LocalNixStore::UnixStream(ref mut store) => {
                store.query_pathinfo(path.clone()).result().await.unwrap_or(None).is_some()
            }
            core::types::LocalNixStore::CommandDuplex(ref mut store) => {
                store.query_pathinfo(path.clone()).result().await.unwrap_or(None).is_some()
            }
        };

        if !path_exists {
            println!("Path {} not found in local store, skipping cache", path);
            return;
        }
    } else {
        println!("Failed to connect to local store, skipping cache for {}", path);
        return;
    }

    let organization_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(organization.id))
        .all(&state.db)
        .await
        .unwrap();

    for organization_cache in organization_caches {
        let ocs = EOrganizationCache::find()
            .filter(COrganizationCache::Cache.eq(organization_cache.cache))
            .all(&state.db)
            .await
            .unwrap();

        for oc in ocs {
            let cache = ECache::find_by_id(oc.cache)
                .one(&state.db)
                .await
                .unwrap()
                .unwrap();

            if cache.active {
                sign_build_output(Arc::clone(&state), cache.clone(), build_output.clone()).await;
            }
        }
    }

    println!(
        "Caching build output: {}-{}",
        build_output.hash, build_output.package
    );
    let pack_result = pack_build_output(Arc::clone(&state), build_output.clone()).await;
    let (file_hash, file_size) = match pack_result {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Failed to pack build output: {}", e);
            return;
        }
    };

    let mut abuild_output = build_output.clone().into_active_model();

    abuild_output.file_hash = Set(Some(file_hash));
    abuild_output.file_size = Set(Some(file_size as i64));
    abuild_output.is_cached = Set(true);

    abuild_output.update(&state.db).await.unwrap();
}

pub async fn sign_build_output(state: Arc<ServerState>, cache: MCache, build_output: MBuildOutput) {
    let path = get_path_from_build_output(build_output.clone());
    let secret_key = format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
        false,
    );

    let key_file = write_key(secret_key.clone(), state.cli.base_path.clone()).unwrap();

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
            eprintln!("Error while executing command: {}", e);
            return;
        }
    };

    if !output.status.success() {
        eprintln!("Could not sign Path");
        return;
    }

    clear_key(key_file).unwrap();
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
            eprintln!("Error while executing command: {}", e);
            return;
        }
    };

    if !output.status.success() {
        eprintln!("Could not get path info");
        return;
    }

    let signatures = String::from_utf8_lossy(&output.stdout).to_string();
    let mut signature = String::new();
    for line in signatures.lines() {
        if line.contains(secret_key.split(':').collect::<Vec<&str>>()[0]) {
            signature = line.split_whitespace().last().unwrap().to_string();
            break;
        }
    }

    let build_path_signature = ABuildOutputSignature {
        id: Set(Uuid::new_v4()),
        build_output: Set(build_output.id),
        cache: Set(cache.id),
        signature: Set(signature),
        created_at: Set(Utc::now().naive_utc()),
    };

    build_path_signature.insert(&state.db).await.unwrap();
}

pub async fn pack_build_output(
    state: Arc<ServerState>,
    build_output: MBuildOutput,
) -> Result<(String, u32), String> {
    let path = get_path_from_build_output(build_output);

    let (path_hash, _path_package) = get_hash_from_path(path.clone()).unwrap();
    let file_location_tmp =
        get_cache_nar_location(state.cli.base_path.clone(), path_hash.clone(), false);
    let file_location =
        get_cache_nar_location(state.cli.base_path.clone(), path_hash.clone(), true);

    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("nar")
        .arg("pack")
        .arg(path)
        .output()
        .await
        .map_err(|e| e.to_string())
    {
        Ok(output) => output,
        Err(e) => {
            return Err(format!("Error while executing command: {}", e));
        }
    };

    if !output.status.success() {
        return Err("Could not pack Path".to_string());
    }

    tokio::fs::write(file_location_tmp.clone(), output.stdout)
        .await
        .map_err(|e| e.to_string())
        .unwrap();

    let output = match Command::new(state.cli.binpath_zstd.clone())
        .arg("-T0")
        .arg("-q")
        .arg("--rm")
        .arg("-f")
        .arg("-19")
        .arg(file_location_tmp.clone())
        .arg("-o")
        .arg(file_location.clone())
        .output()
        .await
        .map_err(|e| e.to_string())
    {
        Ok(output) => output,
        Err(e) => {
            return Err(format!("Error while executing command: {}", e));
        }
    };

    if !output.status.success() {
        return Err("Could not compress Path".to_string());
    }

    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("hash")
        .arg("file")
        .arg("--base32")
        .arg(file_location.clone())
        .output()
        .await
        .map_err(|e| e.to_string())
    {
        Ok(output) => output,
        Err(e) => {
            return Err(format!("Error while executing command: {}", e));
        }
    };

    if !output.status.success() {
        return Err("Could not retrive hash of file".to_string());
    }

    let file_hash = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let file_metadata = std::fs::metadata(file_location).unwrap();
    let file_size = file_metadata.len() as u32;

    Ok((format!("sha256:{}", file_hash), file_size))
}

async fn get_next_build_output(state: Arc<ServerState>) -> Option<MBuildOutput> {
    EBuildOutput::find()
        .filter(CBuildOutput::IsCached.eq(false))
        .order_by_asc(CBuildOutput::CreatedAt)
        .one(&state.db)
        .await
        .unwrap()
}

pub async fn invalidate_cache_for_path(state: Arc<ServerState>, path: String) -> Result<(), String> {
    let (hash, package) = get_hash_from_path(path.clone())
        .map_err(|e| format!("Failed to parse path {}: {}", path, e))?;

    // Find all build outputs for this path
    let build_outputs = EBuildOutput::find()
        .filter(
            Condition::all()
                .add(CBuildOutput::Hash.eq(hash.clone()))
                .add(CBuildOutput::Package.eq(package.clone()))
                .add(CBuildOutput::IsCached.eq(true)),
        )
        .all(&state.db)
        .await
        .map_err(|e| format!("Database error: {}", e))?;

    for build_output in build_outputs {
        // Mark as not cached
        let mut abuild_output = build_output.clone().into_active_model();
        abuild_output.is_cached = Set(false);
        abuild_output.file_hash = Set(None);
        abuild_output.file_size = Set(None);
        abuild_output.update(&state.db).await
            .map_err(|e| format!("Failed to update build output: {}", e))?;

        // Remove cached files
        let file_location = get_cache_nar_location(state.cli.base_path.clone(), hash.clone(), true);
        if std::fs::metadata(&file_location).is_ok() {
            std::fs::remove_file(&file_location)
                .map_err(|e| format!("Failed to remove cached file {}: {}", file_location, e))?;
        }

        // Remove signatures
        let signatures = EBuildOutputSignature::find()
            .filter(CBuildOutputSignature::BuildOutput.eq(build_output.id))
            .all(&state.db)
            .await
            .map_err(|e| format!("Database error: {}", e))?;

        for signature in signatures {
            let asignature = signature.into_active_model();
            asignature.delete(&state.db).await
                .map_err(|e| format!("Failed to delete signature: {}", e))?;
        }

        println!("Invalidated cache for path: {}", path);
    }

    Ok(())
}

pub async fn cleanup_orphaned_cache_files(state: Arc<ServerState>) -> Result<(), String> {
    let cache_dir = format!("{}/nars", state.cli.base_path);
    
    if !std::path::Path::new(&cache_dir).exists() {
        return Ok(());
    }

    let mut orphaned_files = Vec::new();
    
    // Walk through cache directory
    for entry in std::fs::read_dir(&cache_dir)
        .map_err(|e| format!("Failed to read cache directory: {}", e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
        
        if path.is_dir() {
            for subentry in std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read subdirectory: {}", e))?
            {
                let subentry = subentry.map_err(|e| format!("Failed to read subdirectory entry: {}", e))?;
                let file_path = subentry.path();
                
                if file_path.extension().and_then(|s| s.to_str()) == Some("zst") {
                    if let Some(file_name) = file_path.file_stem().and_then(|s| s.to_str()) {
                        if let Some(hash_part) = file_name.strip_suffix(".nar") {
                            let parent_dir = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                            let full_hash = format!("{}{}", parent_dir, hash_part);
                            
                            // Check if this hash exists in database
                            let build_output_exists = EBuildOutput::find()
                                .filter(
                                    Condition::all()
                                        .add(CBuildOutput::Hash.eq(full_hash.clone()))
                                        .add(CBuildOutput::IsCached.eq(true))
                                )
                                .one(&state.db)
                                .await
                                .map_err(|e| format!("Database error: {}", e))?
                                .is_some();
                            
                            if !build_output_exists {
                                orphaned_files.push(file_path);
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Remove orphaned files
    for file_path in orphaned_files {
        if let Err(e) = std::fs::remove_file(&file_path) {
            eprintln!("Failed to remove orphaned cache file {:?}: {}", file_path, e);
        } else {
            println!("Removed orphaned cache file: {:?}", file_path);
        }
    }
    
    Ok(())
}
