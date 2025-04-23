/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use core::types::*;
use tokio::process::Command;
use core::sources::{write_key, clear_key, get_hash_from_path, get_path_from_build_output};
use core::input::vec_to_hex;
use sea_orm::ActiveValue::Set;
use std::sync::Arc;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder,
};
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

pub async fn cache_loop(state: Arc<ServerState>) {
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let build = get_next_build_output(Arc::clone(&state)).await;

        if let Some(build) = build {
            cache_build_output(Arc::clone(&state), build).await;
        } else {
            interval.tick().await;
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
                sign_build_output(
                    Arc::clone(&state),
                    cache.clone(),
                    build_output.clone(),
                ).await;
            }
        }
    }

    let (file_hash, file_size) = pack_build_output(Arc::clone(&state), build_output.clone())
        .await
        .map_err(|e| {
            eprintln!("{}", e);
            return;
        })
        .unwrap();

    let mut abuild_output = build_output.clone().into_active_model();

    abuild_output.file_hash = Set(Some(file_hash));
    abuild_output.file_size = Set(Some(file_size));
    abuild_output.is_cached = Set(true);

    abuild_output.update(&state.db).await.unwrap();
}

pub async fn sign_build_output(state: Arc<ServerState>, cache: MCache, build_output: MBuildOutput) {
    let path = get_path_from_build_output(build_output.clone());
    let key_file = write_key(
        cache.signing_key.clone(),
        state.cli.base_path.clone(),
    ).unwrap();

    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("store")
        .arg("sign")
        .arg("-k")
        .arg(key_file.clone())
        .arg(path.clone())
        .output()
        .await
        .map_err(|e| e.to_string()) {
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
    let signature = String::from_utf8_lossy(&output.stdout).to_string();
    let build_path_signature = ABuildOutputSignature {
        id: Set(Uuid::new_v4()),
        build_output: Set(build_output.id),
        cache: Set(cache.id),
        signature: Set(signature),
        created_at: Set(Utc::now().naive_utc()),
    };

    build_path_signature.insert(&state.db).await.unwrap();
}

pub async fn pack_build_output(state: Arc<ServerState>, build_output: MBuildOutput) -> Result<(String, u32), String> {
    let path = get_path_from_build_output(build_output);

    let (path_hash, _path_package) = get_hash_from_path(path.clone()).unwrap();
    let file_location_tmp = get_cache_nar_location(state.cli.base_path.clone(), path_hash.clone(), true);
    let file_location = get_cache_nar_location(state.cli.base_path.clone(), path_hash.clone(), false);

    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("nar")
        .arg("pack")
        .arg(path)
        .arg(">")
        .arg(file_location_tmp.clone())
        .output()
        .await
        .map_err(|e| e.to_string()) {
            Ok(output) => output,
            Err(e) => {
                return Err(format!("Error while executing command: {}", e));
            }
        };

    if !output.status.success() {
        return Err("Could not pack Path".to_string());
    }


    let output = match Command::new(state.cli.binpath_zstd.clone())
        .arg("-T0")
        .arg("-q")
        .arg("-19")
        .arg(file_location_tmp.clone())
        .arg("-o")
        .arg(file_location.clone())
        .output()
        .await
        .map_err(|e| e.to_string()) {
            Ok(output) => output,
            Err(e) => {
                return Err(format!("Error while executing command: {}", e));
            }
        };

    if !output.status.success() {
        return Err("Could not compress Path".to_string());
    }

    std::fs::remove_file(file_location_tmp.clone()).unwrap();

    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("hash")
        .arg("file")
        .arg("--base32")
        .arg(file_location.clone())
        .output()
        .await
        .map_err(|e| e.to_string()) {
            Ok(output) => output,
            Err(e) => {
                return Err(format!("Error while executing command: {}", e));
            }
        };

    if !output.status.success() {
        return Err("Could not retrive hash of file".to_string());
    }

    let file_hash = String::from_utf8_lossy(&output.stdout).to_string();


    let file_metadata = std::fs::metadata(file_location).unwrap();
    let file_size = file_metadata.len() as u32;

    Ok((file_hash, file_size))
}

pub fn get_cache_nar_location(base_path: String, hash: Vec<u8>, compressed: bool) -> String {
    let hash_hex = vec_to_hex(&hash);
    let hash_hex = hash_hex.as_str();

    std::fs::create_dir_all(format!("{}/{}", base_path, &hash_hex[0..2])).unwrap();

    format!("{}/{}/{}.nar{}", base_path, &hash_hex[0..2], &hash_hex[2..], if compressed { ".zst" } else { "" })
}

async fn get_next_build_output(state: Arc<ServerState>) -> Option<MBuildOutput> {
    EBuildOutput::find()
        .filter(CBuildOutput::IsCached.eq(false))
        .order_by_asc(CBuild::CreatedAt)
        .one(&state.db)
        .await
        .unwrap()
}
