/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use core::sources::{
    clear_key, format_cache_key, get_hash_from_path, get_path_from_build_output, write_key, get_cache_nar_location
};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
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
                sign_build_output(Arc::clone(&state), cache.clone(), build_output.clone()).await;
            }
        }
    }

    println!("Caching build output: {}-{}", build_output.hash, build_output.package);
    let (file_hash, file_size) = pack_build_output(Arc::clone(&state), build_output.clone())
        .await
        .map_err(|e| {
            eprintln!("{}", e);
        })
        .unwrap();

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
