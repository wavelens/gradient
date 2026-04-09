/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::consts::FLAKE_START;
use gradient_core::executer::strip_nix_store_prefix;
use std::collections::{HashMap, HashSet};
use tracing::{debug, error};

use crate::nix_eval::{NixEvaluator, escape_nix_str};

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
///
/// Synchronous — must run inside `spawn_blocking`.
///
/// The evaluator is borrowed (not constructed) so the caller can reuse one
/// `NixEvaluator` per process. Creating multiple evaluators inside the same
/// process touches libnix global state and hangs.
pub(super) fn discover_derivations(
    evaluator: &NixEvaluator,
    repository: &str,
    wildcards: &[String],
) -> Result<Vec<String>> {
    let escaped_repo = escape_nix_str(repository);

    let mut all_derivations: HashSet<String> = HashSet::new();
    let mut partial_derivations: HashMap<String, HashSet<String>> = HashMap::new();

    let wildcards_ref: Vec<&str> = wildcards.iter().map(|s| s.as_str()).collect();

    'outer: for w in wildcards_ref
        .iter()
        .map(|w| split_attr_path(&format!("{}.#", w)))
    {
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

                    let expr = format!("(builtins.getFlake \"{}\").{}", escaped_repo, current_key);
                    debug!(expr = %expr, "evaluating flake attribute");

                    let keys = match evaluator.attr_names(&expr) {
                        Ok(keys) => keys,
                        Err(e) => {
                            debug!(
                                path = %current_key,
                                error = %e,
                                "Skipping attribute path not present in flake"
                            );
                            continue;
                        }
                    };

                    if keys.contains(&"type".to_string()) && type_check {
                        let type_expr = format!(
                            "(builtins.getFlake \"{}\").{}.type",
                            escaped_repo, current_key
                        );
                        debug!(expr = %type_expr, "evaluating type attribute");

                        match evaluator.eval_string(&type_expr) {
                            Ok(type_value) if type_value == "derivation" => {
                                all_derivations.insert(current_key.clone());
                                continue;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                debug!(
                                    path = %current_key,
                                    error = %e,
                                    "Failed to evaluate type attribute"
                                );
                            }
                        }
                    }

                    let selected_keys = keys
                        .iter()
                        .filter(|s| {
                            s.starts_with(key_start)
                                && s.ends_with(key_end)
                                && s.len() >= key_start.len() + key_end.len()
                        })
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

    Ok(all_derivations.into_iter().collect())
}

/// Resolves a flake attribute path to its store derivation path using the
/// embedded Nix evaluator.
///
/// **Synchronous**: must run inside `tokio::task::spawn_blocking`.
pub(super) fn get_derivation_path(
    evaluator: &NixEvaluator,
    flake_ref: &str,
    attr_path: &str,
) -> Result<(String, Vec<String>)> {
    let expr = format!(
        "toString (builtins.getFlake \"{}\").{}.drvPath",
        escape_nix_str(flake_ref),
        attr_path,
    );

    let drv_path = evaluator
        .eval_string(&expr)
        .with_context(|| format!("nix eval drvPath failed for '{}#{}'", flake_ref, attr_path))?;

    Ok((strip_nix_store_prefix(&drv_path), vec![]))
}
