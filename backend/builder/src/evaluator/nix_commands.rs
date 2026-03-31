/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;
use entity::server::Architecture;
use serde_json::Value;
use std::process::Output;
use tokio::process::Command;

/// Extension trait for parsing JSON from nix command output.
pub trait JsonOutput {
    fn json_to_vec(&self) -> anyhow::Result<Vec<String>>;
    fn json_to_string(&self) -> anyhow::Result<String>;
}

impl JsonOutput for Output {
    fn json_to_vec(&self) -> anyhow::Result<Vec<String>> {
        if !self.status.success() {
            anyhow::bail!("{}", String::from_utf8_lossy(&self.stderr));
        }

        let json_output = String::from_utf8_lossy(&self.stdout);
        if json_output.trim().is_empty() {
            anyhow::bail!("Command returned empty output");
        }

        let parsed_json: Value = serde_json::from_str(&json_output).with_context(|| {
            format!(
                "Failed to parse JSON output: '{}', stderr: '{}'",
                json_output,
                String::from_utf8_lossy(&self.stderr)
            )
        })?;

        parsed_json
            .as_array()
            .context("Expected JSON array")?
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or("Expected string in JSON array")
                    .map(|s| s.to_string())
            })
            .collect::<Result<Vec<String>, &str>>()
            .map_err(|e| anyhow::anyhow!("Expected string in JSON array: {}", e))
    }

    fn json_to_string(&self) -> anyhow::Result<String> {
        if !self.status.success() {
            anyhow::bail!("{}", String::from_utf8_lossy(&self.stderr));
        }

        let json_output = String::from_utf8_lossy(&self.stdout);
        if json_output.trim().is_empty() {
            anyhow::bail!("Command returned empty output");
        }

        let parsed_json: Value = serde_json::from_str(&json_output).with_context(|| {
            format!(
                "Failed to parse JSON output: '{}', stderr: '{}'",
                json_output,
                String::from_utf8_lossy(&self.stderr)
            )
        })?;

        parsed_json
            .as_str()
            .context("Expected JSON string")
            .map(|s| s.to_string())
    }
}

/// Resolves a flake output path to its store derivation path via `nix path-info --derivation`.
pub async fn get_derivation_cmd(
    binpath_nix: &str,
    path: &str,
    git_ssh_command: &str,
) -> anyhow::Result<(String, Vec<String>)> {
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

    if !parsed_json.is_object() {
        anyhow::bail!("Expected JSON object but found another type");
    }

    let drv_path = parsed_json
        .as_object()
        .context("Expected JSON object")?
        .keys()
        .next()
        .context("Expected JSON object with Derivation Path")?
        .to_string();

    let input_paths = parsed_json[drv_path.clone()]
        .as_object()
        .context("Expected JSON object with Derivation Path")?
        .get("references")
        .context("Expected JSON object with Derivation Path")?
        .as_array()
        .context("Expected JSON object with Derivation Path")?
        .iter()
        .map(|v| {
            v.as_str()
                .context("Expected string in JSON array")
                .map(|s| s.to_string())
        })
        .collect::<anyhow::Result<Vec<String>>>()?;

    Ok((drv_path, input_paths))
}

/// Queries system architecture and required features for a `.drv` file via `nix derivation show`.
pub async fn get_features_cmd(
    binpath_nix: &str,
    path: &str,
) -> anyhow::Result<(Architecture, Vec<String>)> {
    // TODO: better check for derivation
    if !path.ends_with(".drv") {
        return Ok((Architecture::BUILTIN, vec![]));
    }

    let output = Command::new(binpath_nix)
        .arg("derivation")
        .arg("show")
        .arg(path)
        .output()
        .await
        .context("Failed to execute nix derivation show command")?;

    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }

    let json_output = String::from_utf8_lossy(&output.stdout);
    let parsed_json: Value = serde_json::from_str(&json_output).with_context(|| {
        format!(
            "Failed to parse JSON output from 'nix derivation show {}': '{}', stderr: '{}'",
            path,
            json_output,
            String::from_utf8_lossy(&output.stderr)
        )
    })?;

    if !parsed_json.is_object() {
        anyhow::bail!("Expected JSON object but found another type");
    }

    let drv_obj = parsed_json
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object"))?
        .get(path)
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with path"))?
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with path"))?;

    let env = drv_obj
        .get("env")
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with env"))?
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with env"))?;

    // Some derivations embed a nested JSON string under `__json` instead of inline env attrs.
    let env_resolved: serde_json::Map<String, Value> = if let Some(json_str) = env.get("__json") {
        let s = json_str
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Expected string for __json field"))?;
        serde_json::from_str(s).with_context(|| {
            format!(
                "Failed to parse nested JSON in __json field from 'nix derivation show {}': '{}'",
                path, s
            )
        })?
    } else {
        env.clone()
    };

    let features: Vec<String> = match env_resolved.get("requiredSystemFeatures") {
        Some(pjson) => pjson
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or("Expected string in JSON array")
                    .map(|s| s.to_string())
            })
            .collect::<Result<Vec<String>, &str>>()
            .map_err(|e| anyhow::anyhow!("Invalid system feature: {}", e))?,
        None => vec![],
    };

    let system: Architecture = drv_obj
        .get("system")
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with system"))?
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Expected string for system field"))?
        .try_into()
        .map_err(|e| anyhow::anyhow!("{} has invalid system architecture: {:?}", path, e))?;

    Ok((system, features))
}
