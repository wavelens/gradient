/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::Utc;
use core::database::add_features;
use core::executer::*;
use core::input::{parse_evaluation_wildcard, repository_url_to_nix, vec_to_hex};
use core::sources::prefetch_flake;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use nix_daemon::nix::DaemonStore;
use sea_orm::ActiveValue::Set;
use sea_orm::entity::prelude::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QuerySelect};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::option::Option;
use std::process::Output;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::scheduler::{update_evaluation_status, update_evaluation_status_with_error};
use core::consts::FLAKE_START;

#[instrument(skip(state, store), fields(evaluation_id = %evaluation.id))]
pub async fn evaluate<C: AsyncWriteExt + AsyncReadExt + Unpin + Send>(
    state: Arc<ServerState>,
    store: &mut DaemonStore<C>,
    evaluation: &MEvaluation,
) -> Result<(Vec<MBuild>, Vec<MBuildDependency>)> {
    info!("Starting evaluation");
    update_evaluation_status(
        Arc::clone(&state),
        evaluation.clone(),
        EvaluationStatus::Evaluating,
    )
    .await;

    let organization_id = if let Some(project_id) = evaluation.project {
        EProject::find_by_id(project_id)
            .one(&state.db)
            .await
            .context("Failed to query project")?
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?
            .organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
            .context("Failed to query direct build")?
            .ok_or_else(|| anyhow::anyhow!("Direct build not found"))?
            .organization
    };

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .context("Failed to query organization")?
        .ok_or_else(|| anyhow::anyhow!("Organization not found"))?;

    let commit = ECommit::find_by_id(evaluation.commit)
        .one(&state.db)
        .await
        .context("Failed to query commit")?
        .ok_or_else(|| anyhow::anyhow!("Commit not found"))?;

    let repository =
        repository_url_to_nix(&evaluation.repository, vec_to_hex(&commit.hash).as_str())
            .context("Failed to convert repository URL to Nix format")?;

    prefetch_flake(Arc::clone(&state), repository.clone(), organization.clone())
        .await
        .context("Failed to prefetch flake")?;

    let wildcards = parse_evaluation_wildcard(evaluation.wildcard.as_str())
        .context("Failed to parse evaluation wildcard")?;

    let all_derivations = get_flake_derivations(Arc::clone(&state), repository.clone(), wildcards, organization)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to evaluate: {}", e))?;

    if all_derivations.is_empty() {
        warn!("No derivations found for evaluation");
        return Ok((vec![], vec![]));
    }

    let mut all_builds: Vec<MBuild> = vec![];
    let mut all_dependencies: Vec<MBuildDependency> = vec![];
    let mut failed_derivations: Vec<(String, String)> = vec![];
    let total_derivations = all_derivations.len();

    for derivation_string in all_derivations {
        let path = format!("{}#{}", repository, derivation_string);

        // TODO: use nix api
        let (derivation, _references) =
            match get_derivation_cmd(state.cli.binpath_nix.as_str(), &path).await {
                Ok((d, r)) => (d, r),
                Err(e) => {
                    let error_msg = e.to_string();
                    warn!(
                        error = %e,
                        derivation = %derivation_string,
                        "Derivation failed, skipping broken package"
                    );
                    failed_derivations.push((derivation_string.clone(), error_msg));
                    continue;
                }
            };

        let missing = core::executer::get_missing_builds(vec![derivation.clone()], store).await?;

        if missing.is_empty() {
            debug!(derivation = %derivation, "Skipping package - already in store");

            if let Err(e) = add_existing_build(
                Arc::clone(&state),
                derivation.clone(),
                evaluation.id,
                Uuid::new_v4(),
            )
            .await
            {
                error!(error = %e, "Failed to add existing build");
            }

            continue;
        }

        let already_exsists = all_builds.iter().any(|b| b.derivation_path == derivation);

        let build = vec![derivation.clone()];

        if already_exsists
            || !find_builds(Arc::clone(&state), organization_id, build.clone(), true)
                .await?
                .is_empty()
        {
            debug!(derivation = %derivation, "Skipping package - already exists");
            continue;
        }

        info!(derivation = %derivation, path = %path, "Creating build");

        query_all_dependencies(
            Arc::clone(&state),
            &mut all_builds,
            &mut all_dependencies,
            evaluation,
            organization_id,
            build,
            store,
        )
        .await?;

        debug!(derivation = %derivation, "Successfully processed package");
    }

    if all_builds.is_empty() && !failed_derivations.is_empty() {
        let error_summary = if failed_derivations.len() == total_derivations {
            format!(
                "All {} derivations failed during evaluation",
                total_derivations
            )
        } else {
            format!(
                "{} out of {} derivations failed, no builds created",
                failed_derivations.len(),
                total_derivations
            )
        };

        let detailed_errors: Vec<String> = failed_derivations
            .iter()
            .map(|(deriv, error)| format!("- {}: {}", deriv, error))
            .collect();

        let full_error = format!("{}:\n{}", error_summary, detailed_errors.join("\n"));
        return Err(anyhow::anyhow!(full_error));
    }

    Ok((all_builds, all_dependencies))
}

async fn query_all_dependencies<C: AsyncWriteExt + AsyncReadExt + Unpin + Send>(
    state: Arc<ServerState>,
    all_builds: &mut Vec<MBuild>,
    all_dependencies: &mut Vec<MBuildDependency>,
    evaluation: &MEvaluation,
    organization_id: Uuid,
    dependencies: Vec<String>,
    store: &mut DaemonStore<C>,
) -> Result<()> {
    let mut dependencies = dependencies
        .clone()
        .into_iter()
        .map(|d| (d, None, Uuid::new_v4()))
        .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

    while let Some((dependency, dependency_id, build_id)) = dependencies.pop() {
        debug!(
            derivation = %dependency,
            build_id = %build_id,
            parent_dependency_id = ?dependency_id,
            "Processing derivation"
        );

        let path_info = get_pathinfo(dependency.clone(), store)
            .await
            .context("Failed to get derivation info")?
            .context("Derivation not found in Nix store")?;

        let already_exists = find_builds(
            Arc::clone(&state),
            organization_id,
            path_info.references.clone(),
            false,
        )
        .await
        .context("Failed to find existing builds")?;

        let mut references = path_info.references
            .clone()
            .into_iter()
            .map(|d| (d, Some(build_id), Uuid::new_v4()))
            .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

        let mut check_availablity: Vec<MBuild> = Vec::new();

        references.retain(|d| {
            let d_path = d.0.clone();

            let in_builds = all_builds.iter().any(|b| b.derivation_path == d_path);
            let in_exists = already_exists.iter().find(|b| b.derivation_path == d_path);
            let in_dependencies = dependencies.iter().any(|(path, _, _)| *path == d_path);

            if in_builds || in_dependencies {
                let d_id = if in_builds {
                    match all_builds.iter().find(|b| b.derivation_path == d_path) {
                        Some(build) => build.id,
                        None => {
                            error!("Build not found for path: {}", d_path);
                            return false;
                        }
                    }
                } else {
                    match dependencies.iter().find(|(path, _, _)| *path == d_path) {
                        Some((_, _, id)) => *id,
                        None => {
                            error!("Dependency not found for path: {}", d_path);
                            return false;
                        }
                    }
                };

                let dep = MBuildDependency {
                    id: Uuid::new_v4(),
                    build: build_id,
                    dependency: d_id,
                };

                debug!(build = %build_id, dependency = %d_id, "Creating dependency");

                all_dependencies.push(dep);

                false
            } else if let Some(in_exists) = in_exists {
                check_availablity.push(in_exists.clone());
                true
            } else {
                true
            }
        });

        for b in check_availablity {
            let dep = MBuildDependency {
                id: Uuid::new_v4(),
                build: build_id,
                dependency: b.id,
            };

            debug!(build = %build_id, dependency = %b.id, "Creating dependency for existing build");

            references.retain(|(d, _, _)| *d != b.derivation_path);
            if get_missing_builds(vec![b.derivation_path.clone()], store)
                .await
                .context("Failed to get missing builds")?
                .is_empty()
            {
                let mut abuild: ABuild = b.clone().into();
                if b.status != BuildStatus::Completed {
                    abuild.status = Set(BuildStatus::Completed);
                    abuild.log = Set(None);
                }

                abuild.evaluation = Set(evaluation.id);
                abuild
                    .save(&state.db)
                    .await
                    .context("Failed to save build status")?;
            } else {
                let mut abuild: ABuild = b.into();
                abuild.status = Set(BuildStatus::Queued);
                abuild.log = Set(None);
                abuild.evaluation = Set(evaluation.id);
                abuild
                    .save(&state.db)
                    .await
                    .context("Failed to save build status")?;
            }

            all_dependencies.push(dep);
        }

        let not_missing = get_missing_builds(vec![dependency.clone()], store)
            .await
            .context("Failed to get missing builds")?
            .is_empty();

        if not_missing {
            add_existing_build(
                Arc::clone(&state),
                dependency.clone(),
                evaluation.id,
                build_id,
            ).await?;

            debug!(
                build_id = %build_id,
                derivation_path = %dependency,
                "Skipping package - already in store"
            );
        } else {
            let (system, features) = get_features_cmd(state.cli.binpath_nix.as_str(), dependency.as_str())
                    .await
                    .with_context(|| format!("Failed to get build features for derivation: {}", dependency))?;

            // TODO: add better derivation check
            let build = if dependency.ends_with(".drv") {
                MBuild {
                    id: build_id,
                    evaluation: evaluation.id,
                    derivation_path: dependency.clone(),
                    architecture: system,
                    status: BuildStatus::Created,
                    server: None,
                    log: None,
                    created_at: Utc::now().naive_utc(),
                    updated_at: Utc::now().naive_utc(),
                }
            } else {
                MBuild {
                    id: build_id,
                    evaluation: evaluation.id,
                    derivation_path: dependency.clone(),
                    architecture: system,
                    status: BuildStatus::Completed,
                    server: None,
                    log: None,
                    created_at: Utc::now().naive_utc(),
                    updated_at: Utc::now().naive_utc(),
                }
            };

            if let Err(e) = add_features(Arc::clone(&state), features, Some(build_id), None).await {
                error!(error = %e, "Failed to add features for build");
            }

            debug!(
                build_id = %build.id,
                derivation_path = %build.derivation_path,
                "Creating build"
            );

            all_builds.push(build);
            dependencies.extend(references);
        };

        if let Some(d_id) = dependency_id {
            let dep = MBuildDependency {
                id: Uuid::new_v4(),
                build: d_id,
                dependency: build_id,
            };

            debug!(build = %d_id, dependency = %build_id, "Creating parent dependency");

            all_dependencies.push(dep);
        }
    }
    Ok(())
}

async fn find_builds(
    state: Arc<ServerState>,
    organization_id: Uuid,
    build_paths: Vec<String>,
    successful: bool,
) -> Result<Vec<MBuild>> {
    let mut condition = Condition::any();
    for path in build_paths {
        condition = condition.add(CBuild::DerivationPath.eq(path.as_str()));
    }

    let mut filter = Condition::all()
        .add(CProject::Organization.eq(organization_id))
        .add(condition);

    if successful {
        filter = filter.add(CBuild::Status.eq(BuildStatus::Completed));
    }

    let builds = EBuild::find()
        .join(JoinType::InnerJoin, RBuild::Evaluation.def())
        .join(JoinType::InnerJoin, REvaluation::Project.def())
        .filter(filter)
        .all(&state.db)
        .await
        .context("Failed to query builds")?;

    Ok(builds)
}

pub async fn get_derivation_cmd(
    binpath_nix: &str,
    path: &str,
) -> anyhow::Result<(String, Vec<String>)> {
    let output = Command::new(binpath_nix)
        .arg("path-info")
        .arg("--json")
        .arg("--derivation")
        .arg(path)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }

    let json_output = String::from_utf8_lossy(&output.stdout);
    let parsed_json: Value = serde_json::from_str(&json_output).with_context(|| format!("Failed to parse JSON output from 'nix path-info --derivation {}': '{}', stderr: '{}'", path, json_output, String::from_utf8_lossy(&output.stderr)))?;

    if !parsed_json.is_object() {
        anyhow::bail!("Expected JSON object but found another type");
    }

    let path = parsed_json
        .as_object()
        .context("Expected JSON object")?
        .keys()
        .next()
        .context("Expected JSON object with Derivation Path")?
        .to_string();

    let input_paths = parsed_json[path.clone()]
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

    Ok((path, input_paths))
}

pub async fn get_features_cmd(
    binpath_nix: &str,
    path: &str,
) -> anyhow::Result<(entity::server::Architecture, Vec<String>)> {
    // TODO: better check for derivation
    if !path.ends_with(".drv") {
        return Ok((entity::server::Architecture::BUILTIN, vec![]));
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
    let parsed_json: Value =
        serde_json::from_str(&json_output).with_context(|| format!("Failed to parse JSON output from 'nix derivation show {}': '{}', stderr: '{}'", path, json_output, String::from_utf8_lossy(&output.stderr)))?;

    if !parsed_json.is_object() {
        anyhow::bail!("Expected JSON object but found another type");
    }

    let parsed_json = parsed_json
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object"))?
        .get(path)
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with path"))?
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with path"))?;

    let parsed_json_env = parsed_json
        .get("env")
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with env"))?
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with env"))?;

    let parsed_json_env = if let Some(new_json) = parsed_json_env.get("__json") {
        let new_json = new_json
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Expected string for __json field"))?;
        serde_json::from_str(new_json).with_context(|| format!("Failed to parse nested JSON in __json field from 'nix derivation show {}': '{}'", path, new_json))?
    } else {
        parsed_json_env.clone()
    };

    let features: Vec<String> = if let Some(pjson) = parsed_json_env.get("requiredSystemFeatures") {
        pjson
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or("Expected string in JSON array")
                    .map(|s| s.to_string())
            })
            .collect::<Result<Vec<String>, &str>>()
            .map_err(|e| anyhow::anyhow!("Invalid system feature: {}", e))?
    } else {
        vec![]
    };

    let system: entity::server::Architecture = parsed_json
        .get("system")
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with system"))?
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Expected string for system field"))?
        .try_into()
        .map_err(|e| anyhow::anyhow!("{} has invalid system architecture: {:?}", path, e))?;

    Ok((system, features))
}

async fn add_existing_build(
    state: Arc<ServerState>,
    derivation: String,
    evaluation_id: Uuid,
    build_id: Uuid,
) -> Result<MBuild> {
    let (system, features) = get_features_cmd(state.cli.binpath_nix.as_str(), derivation.as_str())
        .await?;

    let abuild = ABuild {
        id: Set(build_id),
        evaluation: Set(evaluation_id),
        derivation_path: Set(derivation.clone()),
        architecture: Set(system),
        status: Set(BuildStatus::Completed),
        server: Set(None),
        log: Set(None),
        created_at: Set(Utc::now().naive_utc()),
        updated_at: Set(Utc::now().naive_utc()),
    };

    let build = abuild
        .insert(&state.db)
        .await
        .context("Failed to insert build")?;

    if let Err(e) = add_features(Arc::clone(&state), features, Some(build.id), None).await {
        error!(error = %e, "Failed to add features for build");
    }

    let local_store = get_local_store(None)
        .await
        .context("Failed to get local store")?;

    let outputs = match local_store {
        LocalNixStore::UnixStream(mut store) => {
            core::executer::get_build_outputs_from_derivation(derivation.clone(), &mut store).await
        }
        LocalNixStore::CommandDuplex(mut store) => {
            core::executer::get_build_outputs_from_derivation(derivation.clone(), &mut store).await
        }
    };

    if let Ok(outputs) = outputs {
        for output in outputs {
            let abuild_output = ABuildOutput {
                id: Set(Uuid::new_v4()),
                build: Set(build.id),
                name: Set(output.name.clone()),
                output: Set(output.path.clone()),
                hash: Set(output.hash),
                package: Set(output.package),
                file_hash: Set(None),
                file_size: Set(None),
                is_cached: Set(false),
                ca: Set(output.ca),
                created_at: Set(Utc::now().naive_utc()),
            };

            abuild_output
                .insert(&state.db)
                .await
                .context("Failed to insert build output")?;
        }
    }

    Ok(build)
}

async fn get_flake_derivations(
    state: Arc<ServerState>,
    repository: String,
    wildcards: Vec<&str>,
    organization: MOrganization,
) -> Result<Vec<String>> {
    use core::sources::{decrypt_ssh_private_key, write_key, clear_key};

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
        format!("{}.#", w)
            .split(".")
            .map(|s| s.to_string())
            .collect::<Vec<String>>()
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
                        let type_eval_target = format!("{}#{}.type", repository.clone(), current_key);
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

trait JsonOutput {
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

        let parsed_json: Value = serde_json::from_str(&json_output)
            .with_context(|| format!("Failed to parse JSON output: '{}', stderr: '{}'", json_output, String::from_utf8_lossy(&self.stderr)))?;

        let parsed_json = parsed_json
            .as_array()
            .context("Expected JSON array")?
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or("Expected string in JSON array")
                    .map(|s| s.to_string())
            })
            .collect::<Result<Vec<String>, &str>>()
            .map_err(|e| anyhow::anyhow!("Expected string in JSON array: {}", e))?;

        Ok(parsed_json)
    }

    fn json_to_string(&self) -> anyhow::Result<String> {
        if !self.status.success() {
            anyhow::bail!("{}", String::from_utf8_lossy(&self.stderr));
        }


        let json_output = String::from_utf8_lossy(&self.stdout);
        if json_output.trim().is_empty() {
            anyhow::bail!("Command returned empty output");
        }

        let parsed_json: Value = serde_json::from_str(&json_output)
            .with_context(|| format!("Failed to parse JSON output: '{}', stderr: '{}'", json_output, String::from_utf8_lossy(&self.stderr)))?;

        let parsed_json = parsed_json
            .as_str()
            .context("Expected JSON string")?
            .to_string();

        Ok(parsed_json)
    }
}

pub async fn evaluate_direct(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    temp_dir: String,
) -> Result<()> {
    info!(evaluation_id = %evaluation.id, "Starting direct evaluation");
    let local_store = get_local_store(None)
        .await
        .context("Failed to get local store for direct evaluation")?;

    let mut direct_evaluation = evaluation.clone();
    direct_evaluation.repository = temp_dir.clone();

    let evaluation_result = match local_store {
        LocalNixStore::UnixStream(mut store) => {
            evaluate(Arc::clone(&state), &mut store, &direct_evaluation).await
        }
        LocalNixStore::CommandDuplex(mut store) => {
            evaluate(Arc::clone(&state), &mut store, &direct_evaluation).await
        }
    };

    match evaluation_result {
        Ok((builds, dependencies)) => {
            info!(
                build_count = builds.len(),
                dependency_count = dependencies.len(),
                "Direct evaluation completed successfully"
            );

            let active_builds = builds
                .iter()
                .map(|b| b.clone().into_active_model())
                .collect::<Vec<ABuild>>();
            let active_dependencies = dependencies
                .iter()
                .map(|d| d.clone().into_active_model())
                .collect::<Vec<ABuildDependency>>();

            if !active_builds.is_empty() {
                const BATCH_SIZE: usize = 1000;
                for chunk in active_builds.chunks(BATCH_SIZE) {
                    EBuild::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert builds")?;
                }
            }

            if !active_dependencies.is_empty() {
                const BATCH_SIZE: usize = 1000;
                for chunk in active_dependencies.chunks(BATCH_SIZE) {
                    EBuildDependency::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert dependencies")?;
                }
            }

            for build in builds {
                crate::scheduler::update_build_status(
                    Arc::clone(&state),
                    build,
                    BuildStatus::Queued,
                )
                .await;
            }

            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Building)
                .await;

            if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
                warn!(error = %e, temp_dir = %temp_dir, "Failed to cleanup temp directory");
            }

            Ok(())
        }
        Err(e) => {
            error!(error = %e, "Direct evaluation failed");
            update_evaluation_status_with_error(
                Arc::clone(&state),
                evaluation,
                EvaluationStatus::Failed,
                format!("Direct evaluation failed: {}", e),
            )
            .await;

            if let Err(cleanup_err) = tokio::fs::remove_dir_all(&temp_dir).await {
                warn!(error = %cleanup_err, temp_dir = %temp_dir, "Failed to cleanup temp directory after evaluation failure");
            }

            Err(e)
        }
    }
}
