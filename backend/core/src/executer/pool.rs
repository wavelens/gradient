/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use harmonia_utils_hash::fmt::CommonHash as _;
use std::collections::HashMap;

use crate::executer::path_utils::{nix_store_path, strip_store_prefix};
use crate::sources::get_hash_from_path;

pub use harmonia_store_remote::pool::{ConnectionPool, PoolConfig, PooledConnectionGuard};

// ── Nix store helper functions ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BuildOutputInfo {
    pub name: String,
    pub path: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
}

pub async fn get_missing_builds(pool: &ConnectionPool, paths: Vec<String>) -> Result<Vec<String>> {
    let mut output_paths: HashMap<String, String> = HashMap::new();
    let mut drv_paths: Vec<String> = Vec::new();

    for path in paths {
        if path.ends_with(".drv") {
            drv_paths.push(path);
        } else {
            output_paths.insert(path.clone(), nix_store_path(&path));
        }
    }

    if !drv_paths.is_empty() {
        let mut tasks: FuturesUnordered<_> = drv_paths
            .into_iter()
            .map(|path| async move {
                let mut guard = pool
                    .acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("acquire store for output map: {}", e))?;
                let full_path = nix_store_path(&path);
                let output_map = get_output_paths_internal(full_path.clone(), guard.client())
                    .await
                    .with_context(|| format!("Failed to get output path for {}", full_path))?;
                anyhow::Ok((path, output_map))
            })
            .collect();

        while let Some(result) = tasks.next().await {
            let (path, output_map) = result?;
            for out_path in output_map.values() {
                output_paths.insert(path.clone(), out_path.clone());
            }
        }
    }

    let mut guard = pool
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire store for valid paths: {}", e))?;

    let store_paths: harmonia_store_core::store_path::StorePathSet = output_paths
        .values()
        .filter_map(|p| StorePath::from_base_path(strip_store_prefix(p)).ok())
        .collect();

    let valid_paths = guard
        .client()
        .query_valid_paths(&store_paths, true)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to query valid paths: {}", e))?;

    let valid_strings: std::collections::HashSet<String> = valid_paths
        .iter()
        .map(|p| format!("/nix/store/{}", p))
        .collect();

    let missing = output_paths
        .into_iter()
        .filter(|(_, v)| !valid_strings.contains(v))
        .map(|(k, _)| k)
        .collect();

    Ok(missing)
}

async fn get_output_paths_internal<R, W>(
    path: String,
    store: &mut harmonia_store_remote::DaemonClient<R, W>,
) -> Result<HashMap<String, String>>
where
    R: tokio::io::AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
{
    let store_path = StorePath::from_base_path(strip_store_prefix(&path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", path, e))?;
    let output_map = store
        .query_derivation_output_map(&store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_derivation_output_map failed: {}", e))?;
    Ok(output_map
        .into_iter()
        .filter_map(|(name, sp_opt)| {
            sp_opt.map(|sp| (name.to_string(), format!("/nix/store/{}", sp)))
        })
        .collect())
}

pub async fn get_pathinfo(
    path: String,
    guard: &mut PooledConnectionGuard,
) -> Result<Option<PathInfo>> {
    let store_path = StorePath::from_base_path(strip_store_prefix(&path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", path, e))?;
    let info = guard
        .client()
        .query_path_info(&store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?;
    Ok(info.map(|vi| convert_valid_path_info(&vi)))
}

pub async fn get_build_outputs_from_derivation(
    derivation_path: String,
    guard: &mut PooledConnectionGuard,
) -> Result<Vec<BuildOutputInfo>> {
    let drv_store_path = StorePath::from_base_path(strip_store_prefix(&derivation_path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", derivation_path, e))?;
    let output_map = guard
        .client()
        .query_derivation_output_map(&drv_store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_derivation_output_map failed: {}", e))?;

    let mut outputs = Vec::new();
    for (output_name, output_store_path_opt) in &output_map {
        let Some(output_store_path) = output_store_path_opt else {
            continue;
        };
        let output_path_str = format!("/nix/store/{}", output_store_path);
        if let Some(vi) = guard
            .client()
            .query_path_info(output_store_path)
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?
        {
            let (hash, package) = get_hash_from_path(output_path_str.clone())
                .with_context(|| format!("Failed to parse path {}", output_path_str))?;
            outputs.push(BuildOutputInfo {
                name: output_name.to_string(),
                path: output_path_str,
                hash,
                package,
                ca: vi.ca.as_ref().map(|ca| ca.to_string()),
            });
        }
    }
    Ok(outputs)
}

/// Gradient's own `PathInfo` — a thin, string-based representation of the
/// fields consumers actually need. Avoids leaking harmonia protocol types
/// throughout the codebase.
#[derive(Debug, Clone)]
pub struct PathInfo {
    pub deriver: Option<String>,
    pub references: Vec<String>,
    /// NAR hash in SRI format, e.g. `sha256-<base64>`.
    pub nar_hash: String,
    pub nar_size: u64,
    pub ultimate: bool,
    pub signatures: Vec<String>,
    pub ca: Option<String>,
}

/// Convert harmonia's `UnkeyedValidPathInfo` into our local `PathInfo`.
pub fn convert_valid_path_info(vi: &harmonia_store_remote::UnkeyedValidPathInfo) -> PathInfo {
    PathInfo {
        deriver: vi.deriver.as_ref().map(|d| d.to_string()),
        references: vi.references.iter().map(|r| r.to_string()).collect(),
        nar_hash: format!("{}", vi.nar_hash.sri()),
        nar_size: vi.nar_size,
        ultimate: vi.ultimate,
        signatures: vi.signatures.iter().map(|s| s.to_string()).collect(),
        ca: vi.ca.as_ref().map(|ca| ca.to_string()),
    }
}
