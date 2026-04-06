/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;
use core::executer::{nix_store_path, strip_nix_store_prefix};
use entity::server::Architecture;

/// Resolves a flake output path to its store derivation path via the Nix C API.
///
/// `path` must be of the form `"flake_ref#attr.path"`, e.g.
/// `"git+https://...?rev=abc#packages.x86_64-linux.hello"`.
pub async fn get_derivation_cmd(path: &str) -> anyhow::Result<(String, Vec<String>)> {
    let (flake_ref, attr_path) = path
        .split_once('#')
        .with_context(|| format!("path '{}' does not contain '#'", path))?;

    let flake_ref = flake_ref.to_string();
    let attr_path = attr_path.to_string();

    let drv_path = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let evaluator = super::nix_eval::NixEvaluator::new()
            .context("Failed to create Nix evaluator")?;
        let expr = format!(
            "toString (builtins.getFlake \"{}\").{}.drvPath",
            super::nix_eval::escape_nix_str(&flake_ref),
            attr_path,
        );
        evaluator
            .eval_string(&expr)
            .with_context(|| format!("failed to evaluate drvPath for '{}'", attr_path))
    })
    .await
    .context("spawn_blocking panicked")??;

    Ok((strip_nix_store_prefix(&drv_path), vec![]))
}

/// Reads and parses a `.drv` file, returning the full [`Derivation`].
pub async fn get_derivation(path: &str) -> anyhow::Result<super::drv::Derivation> {
    let full_path = nix_store_path(path);
    let bytes = tokio::fs::read(&full_path)
        .await
        .with_context(|| format!("Failed to read derivation file: {}", full_path))?;

    super::drv::parse_drv(&bytes)
        .with_context(|| format!("Failed to parse derivation {}", path))
}

/// Reads and parses a `.drv` file to extract system architecture and required features.
pub async fn get_features(path: &str) -> anyhow::Result<(Architecture, Vec<String>)> {
    if !path.ends_with(".drv") {
        return Ok((Architecture::BUILTIN, vec![]));
    }

    let drv = get_derivation(path).await?;
    let features = drv.required_system_features();
    let system: Architecture = drv
        .system
        .as_str()
        .try_into()
        .map_err(|e| anyhow::anyhow!("{} has invalid system architecture: {:?}", path, e))?;

    Ok((system, features))
}
