/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;
use core::executer::{nix_store_path, strip_nix_store_prefix};
use entity::server::Architecture;
use serde_json::Value;
use tokio::process::Command;
use tracing::debug;

/// Resolves a flake output path to its store derivation path via `nix path-info --derivation`.
pub async fn get_derivation_cmd(
    binpath_nix: &str,
    path: &str,
    git_ssh_command: &str,
) -> anyhow::Result<(String, Vec<String>)> {
    debug!(cmd = %format!("{} path-info --json --derivation {}", binpath_nix, path), "executing nix command");
    let output = Command::new(binpath_nix)
        .arg("path-info")
        .arg("--json")
        .arg("--derivation")
        .arg(path)
        .env("GIT_SSH_COMMAND", git_ssh_command)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }

    let json_output = String::from_utf8_lossy(&output.stdout);
    let parsed_json: Value = serde_json::from_str(&json_output).with_context(|| {
        format!(
            "Failed to parse JSON output from 'nix path-info --derivation {}': '{}', stderr: '{}'",
            path,
            json_output,
            String::from_utf8_lossy(&output.stderr)
        )
    })?;

    let drv_path_raw = parsed_json
        .as_object()
        .context("nix path-info: expected top-level JSON object")?
        .keys()
        .next()
        .context("nix path-info: output object was empty")?
        .to_string();

    let input_paths = parsed_json[drv_path_raw.clone()]
        .as_object()
        .context("nix path-info: derivation entry is not an object")?
        .get("references")
        .context("nix path-info: missing 'references' field")?
        .as_array()
        .context("nix path-info: 'references' is not an array")?
        .iter()
        .map(|v| {
            v.as_str()
                .context("nix path-info: reference entry is not a string")
                .map(strip_nix_store_prefix)
        })
        .collect::<anyhow::Result<Vec<String>>>()?;

    Ok((strip_nix_store_prefix(&drv_path_raw), input_paths))
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
