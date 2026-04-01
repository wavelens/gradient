/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use core::consts::FLAKE_START;
use core::sources::{clear_key, decrypt_ssh_private_key, write_key};
use core::types::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::process::Command;
use tracing::error;

use super::nix_commands::JsonOutput;

/// Splits a Nix attribute path on `.`, respecting double-quoted segments.
/// `packages.x86_64-linux."python3.12".*` → `["packages", "x86_64-linux", "\"python3.12\"", "*"]`
fn split_attr_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in path.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '.' if !in_quotes => {
                segments.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    segments.push(current);
    segments
}

/// Expands wildcard patterns against a flake's attribute tree and returns all matching derivation
/// paths (e.g. `packages.x86_64-linux.hello`).
pub(super) async fn get_flake_derivations(
    state: Arc<ServerState>,
    repository: String,
    wildcards: Vec<&str>,
    organization: MOrganization,
) -> Result<Vec<String>> {
    let (private_key, _public_key) =
        decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization)?;

    let ssh_key_path = write_key(private_key)?;
    let git_ssh_command = format!(
        "{} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null",
        state.cli.binpath_ssh, ssh_key_path
    );

    let mut all_derivations: HashSet<String> = HashSet::new();
    // let mut all_keys: HashMap<String, HashSet<String>> = HashMap::new(); add this line when
    // optimizing partial_derivations
    let mut partial_derivations: HashMap<String, HashSet<String>> = HashMap::new();

    'outer: for w in wildcards.iter().map(|w| {
        split_attr_path(&format!("{}.#", w))
    }) {
        for (it, t) in w.iter().enumerate() {
            if t.contains("*") || t.contains("#") {
                let mut type_check = false;
                let t = if t == "#" {
                    type_check = true;
                    t.replace("#", "*").clone()
                } else {
                    t.clone()
                };

                // TODO: any number of splits
                let key_split = t.split("*").collect::<Vec<&str>>();
                let (key_start, key_end) = (key_split[0], key_split[1]);
                if it == 0 {
                    let selected_keys = FLAKE_START
                        .map(|s| s.to_string())
                        .to_vec()
                        .iter()
                        .filter(|s| {
                            s.starts_with(key_start)
                                && s.ends_with(key_end)
                                && s.len() >= key_start.len() + key_end.len()
                        })
                        .cloned()
                        .collect::<Vec<String>>();

                    partial_derivations
                        .entry("#".to_string())
                        .and_modify(|s| {
                            selected_keys.iter().for_each(|v| {
                                s.insert(v.clone());
                            });
                        })
                        .or_insert(HashSet::from_iter(selected_keys.iter().cloned()));
                    continue;
                }

                let mut key = vec![0; it];
                let mut run_done = false;
                loop {
                    let mut current_key = Vec::new();
                    for (ik, mut k) in key.clone().into_iter().enumerate() {
                        let val = if ik == 0 {
                            if let Some(derivs) = partial_derivations.get("#") {
                                derivs.iter().collect::<Vec<&String>>()
                            } else {
                                error!("Failed to get partial derivations for '#'");
                                continue 'outer;
                            }
                        } else if w[ik].contains("*") || w[ik].contains("#") {
                            if let Some(derivs) = partial_derivations.get(&current_key.join(".")) {
                                derivs.iter().collect::<Vec<&String>>()
                            } else {
                                error!(
                                    "Failed to get partial derivations for key: {}",
                                    current_key.join(".")
                                );
                                continue 'outer;
                            }
                        } else {
                            vec![&w[ik]]
                        };

                        if k >= val.len() {
                            if ik == 0 {
                                run_done = true;
                                break;
                            }

                            key[ik - 1] += 1;
                            key[ik] = 0;
                            k = 0;
                        }

                        if let Some(v) = val.get(k) {
                            current_key.push(v.as_str());
                        } else {
                            error!("Failed to get value at index {} from derivations", k);
                            continue 'outer;
                        }

                        if ik == key.len() - 1 {
                            key[ik] += 1;
                        }
                    }

                    if run_done {
                        break;
                    }

                    let current_key = current_key.join(".");

                    // TODO: optimize partial_derivations by saving all keys; continue here if
                    // all_keys contains current_key
                    if all_derivations.contains(&current_key) {
                        continue;
                    }

                    let eval_target = format!("{}#{}", repository.clone(), current_key);
                    let keys = Command::new(state.cli.binpath_nix.clone())
                        .arg("eval")
                        .arg(&eval_target)
                        .arg("--apply")
                        .arg("builtins.attrNames")
                        .arg("--json")
                        .env("GIT_SSH_COMMAND", &git_ssh_command)
                        .output()
                        .await?
                        .json_to_vec()?;

                    if keys.contains(&"type".to_string()) && type_check {
                        let type_eval_target =
                            format!("{}#{}.type", repository.clone(), current_key);
                        let type_value = Command::new(state.cli.binpath_nix.clone())
                            .arg("eval")
                            .arg(&type_eval_target)
                            .arg("--json")
                            .env("GIT_SSH_COMMAND", &git_ssh_command)
                            .output()
                            .await?
                            .json_to_string()?;

                        if type_value == "derivation" {
                            all_derivations.insert(current_key.clone());
                            continue;
                        }
                    }

                    let selected_keys = keys
                        .iter()
                        .filter(|s| {
                            s.starts_with(key_start)
                                && s.ends_with(key_end)
                                && s.len() >= key_start.len() + key_end.len()
                        })
                        .cloned()
                        .map(|s| format!("\"{}\"", s))
                        .collect::<Vec<String>>();

                    partial_derivations
                        .entry(current_key.clone())
                        .and_modify(|s| {
                            selected_keys.iter().for_each(|v| {
                                s.insert(v.clone());
                            });
                        })
                        .or_insert(HashSet::from_iter(selected_keys.iter().cloned()));
                }
            } else if !FLAKE_START.iter().any(|s| s == t) && it == 0 {
                break;
            } else if it == 0 {
                let mut new_hashset = HashSet::new();
                new_hashset.insert(t.to_string());
                partial_derivations.insert("#".to_string(), new_hashset);
            }
        }
    }

    clear_key(ssh_key_path).ok();

    Ok(all_derivations.into_iter().collect())
}
