/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::Utc;
use core::executer::*;
use core::sources::*;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::server::Architecture;
use futures::stream::{self, StreamExt};
use nix_daemon::{BasicDerivation, DerivationOutput};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::evaluator::*;

async fn parse_derivation_file(
    binpath_nix: &str,
    derivation_path: &str
) -> anyhow::Result<(String, Vec<String>, HashMap<String, String>, HashSet<String>)> {
    if !derivation_path.ends_with(".drv") {
        return Ok((
            "/bin/bash".to_string(),
            vec![],
            HashMap::new(),
            HashSet::new(),
        ));
    }

    let output = Command::new(binpath_nix)
        .arg("derivation")
        .arg("show")
        .arg(derivation_path)
        .output()
        .await
        .context("Failed to execute nix derivation show command")?;

    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }

    let json_output = String::from_utf8_lossy(&output.stdout);
    let parsed_json: serde_json::Value =
        serde_json::from_str(&json_output).with_context(|| format!("Failed to parse JSON output from 'nix derivation show {}': '{}', stderr: '{}'", derivation_path, json_output, String::from_utf8_lossy(&output.stderr)))?;

    let derivation_data = parsed_json
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object"))?
        .get(derivation_path)
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with path"))?
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object with path"))?;

    let builder = derivation_data
        .get("builder")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let args = derivation_data
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let mut env: HashMap<String, String> = derivation_data
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s)))
                // SECURITY: Filter out __json - it must not be in env per Nix C++ code
                .filter(|(k, _)| k.as_str() != "__json")
                .map(|(k, s)| (
                    k.to_string(),
                    s.to_string()
                ))
                .collect()
        })
        .unwrap_or_default();

    // Handle structured attributes: serialize to JSON and add as __json to env
    // This matches Nix C++ behavior where structuredAttrs is merged into env during serialization
    // ONLY add __json when we have legitimate structuredAttrs
    if let Some(structured_attrs) = derivation_data.get("structuredAttrs") {
        // Serialize structured attrs to JSON string and add to env as __json
        let json_str = serde_json::to_string(structured_attrs)
            .context("Failed to serialize structured attributes")?;
        env.insert("__json".to_string(), json_str);
    }

    let input_srcs = derivation_data
        .get("inputSrcs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    Ok((builder, args, env, input_srcs))
}

async fn create_basic_derivation(
    build: &MBuild,
    local_daemon: &mut LocalNixStore,
    dependencies: Vec<String>,
    state: Arc<ServerState>,
) -> anyhow::Result<BasicDerivation> {
    let out_paths = match local_daemon {
        LocalNixStore::UnixStream(store) => {
            get_output_paths(build.derivation_path.clone(), store).await?
        }
        LocalNixStore::CommandDuplex(store) => {
            get_output_paths(build.derivation_path.clone(), store).await?
        }
    };

    let (builder, args, env, input_srcs) = parse_derivation_file(
        state.cli.binpath_nix.as_str(),
        &build.derivation_path,
    )
    .await
    .context("Failed to parse derivation file")?;

    let mut outputs = HashMap::new();
    for (name, path) in out_paths {
        outputs.insert(name, DerivationOutput {
            path: Some(path),
            hash_algo: None,
            hash: None,
        });
    }

    let input_srcs: HashSet<String> = dependencies.into_iter().chain(input_srcs.into_iter()).collect();

    Ok(BasicDerivation {
        outputs,
        input_srcs: input_srcs.into_iter().collect(),
        platform: build.architecture.to_string(),
        builder,
        args,
        env,  // env now contains __json if structured attrs exist
    })
}

pub async fn schedule_evaluation_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        // TODO: look at tokio semaphore
        while current_schedules.len() < state.cli.max_concurrent_evaluations {
            let evaluation = get_next_evaluation(Arc::clone(&state)).await;
            let schedule = tokio::spawn(schedule_evaluation(Arc::clone(&state), evaluation));
            current_schedules.push(schedule);
            added_schedule = true;
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

#[instrument(skip(state), fields(evaluation_id = %evaluation.id))]
pub async fn schedule_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    info!("Reviewing evaluation");

    let (_project, organization) = if let Some(project_id) = evaluation.project {
        let project = match EProject::find_by_id(project_id).one(&state.db).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                error!("Project not found: {}", project_id);
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    "Project not found".to_string(),
                )
                .await;
                return;
            }
            Err(e) => {
                error!(error = %e, "Failed to query project");
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    format!("Failed to query project: {}", e),
                )
                .await;
                return;
            }
        };

        let organization = match EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
        {
            Ok(Some(o)) => o,
            Ok(None) => {
                error!("Organization not found: {}", project.organization);
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    "Organization not found".to_string(),
                )
                .await;
                return;
            }
            Err(e) => {
                error!(error = %e, "Failed to query organization");
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    format!("Failed to query organization: {}", e),
                )
                .await;
                return;
            }
        };
        (Some(project), organization)
    } else {
        let direct_build = match EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
        {
            Ok(Some(d)) => d,
            Ok(None) => {
                error!("Direct build not found for evaluation: {}", evaluation.id);
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    "Direct build not found".to_string(),
                )
                .await;
                return;
            }
            Err(e) => {
                error!(error = %e, "Failed to query direct build");
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    format!("Failed to query direct build: {}", e),
                )
                .await;
                return;
            }
        };

        let organization = match EOrganization::find_by_id(direct_build.organization)
            .one(&state.db)
            .await
        {
            Ok(Some(o)) => o,
            Ok(None) => {
                error!("Organization not found: {}", direct_build.organization);
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    "Organization not found".to_string(),
                )
                .await;
                return;
            }
            Err(e) => {
                error!(error = %e, "Failed to query organization");
                update_evaluation_status_with_error(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Failed,
                    format!("Failed to query organization: {}", e),
                )
                .await;
                return;
            }
        };
        (None, organization)
    };

    let local_daemon = match get_local_store(Some(organization)).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to get local store");
            update_evaluation_status_with_error(
                Arc::clone(&state),
                evaluation,
                EvaluationStatus::Failed,
                format!("Failed to get local store: {}", e),
            )
            .await;
            return;
        }
    };

    let builds = match local_daemon {
        LocalNixStore::UnixStream(mut store) => {
            evaluate(Arc::clone(&state), &mut store, &evaluation).await
        }
        LocalNixStore::CommandDuplex(mut store) => {
            evaluate(Arc::clone(&state), &mut store, &evaluation).await
        }
    };

    match builds {
        Ok(builds) => {
            let (builds, dependencies) = builds;
            let active_builds = builds
                .iter()
                .map(|b| b.clone().into_active_model())
                .collect::<Vec<ABuild>>();
            let active_dependencies = dependencies
                .iter()
                .map(|d| d.clone().into_active_model())
                .collect::<Vec<ABuildDependency>>();

            info!(
                build_count = builds.len(),
                dependency_count = dependencies.len(),
                "Created builds and dependencies"
            );

            for build in &builds {
                debug!(build_id = %build.id, derivation_path = %build.derivation_path, "Created build");
            }

            for dep in &dependencies {
                debug!(build = %dep.build, dependency = %dep.dependency, "Created dependency");
            }

            if active_builds.is_empty() {
                update_evaluation_status(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Completed,
                )
                .await;
                return;
            }

            const BUILD_BATCH_SIZE: usize = 1000;
            for chunk in active_builds.chunks(BUILD_BATCH_SIZE) {
                if let Err(e) = EBuild::insert_many(chunk.to_vec()).exec(&state.db).await {
                    error!(error = %e, "Failed to insert builds");
                    update_evaluation_status_with_error(
                        Arc::clone(&state),
                        evaluation,
                        EvaluationStatus::Failed,
                        format!("Failed to insert builds: {}", e),
                    )
                    .await;
                    return;
                }
            }

            if !active_dependencies.is_empty() {
                const BATCH_SIZE: usize = 1000;
                for chunk in active_dependencies.chunks(BATCH_SIZE) {
                    if let Err(e) = EBuildDependency::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert build dependencies");
                        update_evaluation_status_with_error(
                            Arc::clone(&state),
                            evaluation,
                            EvaluationStatus::Failed,
                            format!("Failed to insert build dependencies: {}", e),
                        )
                        .await;
                        return;
                    }
                }

                debug!(
                    count = dependencies.len(),
                    "Successfully inserted build dependencies into database"
                );
            } else {
                debug!("No dependencies to insert for evaluation");
            }

            for build in builds {
                update_build_status(Arc::clone(&state), build, BuildStatus::Queued).await;
            }

            info!("Starting evaluation build phase");
            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Building)
                .await;
        }

        Err(e) => {
            error!(error = %e, "Failed to evaluate");
            update_evaluation_status_with_error(
                Arc::clone(&state),
                evaluation,
                EvaluationStatus::Failed,
                format!("{}", e),
            )
            .await;
        }
    }
}

pub async fn schedule_build_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    info!("Build scheduler loop started");

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        while current_schedules.len() < state.cli.max_concurrent_builds {
            let build = get_next_build(Arc::clone(&state)).await;
            debug!(build_id = %build.id, derivation = %build.derivation_path, "Processing build from queue");

            if let Some((build, server)) =
                reserve_available_server(Arc::clone(&state), &build).await
            {
                info!(server_id = %server.id, build_id = %build.id, "Reserving server for build");
                let schedule = tokio::spawn(schedule_build(Arc::clone(&state), build, server));
                current_schedules.push(schedule);
                added_schedule = true;
            }
        }

        if !added_schedule {
            debug!("No builds scheduled this cycle, waiting 5 seconds");
            interval.tick().await;
        }
    }
}

#[instrument(skip(state), fields(build_id = %build.id, server_id = %server.id, derivation_path = %build.derivation_path))]
pub async fn schedule_build(state: Arc<ServerState>, mut build: MBuild, server: MServer) {
    info!("Executing build");

    let organization = match EOrganization::find_by_id(server.organization)
        .one(&state.db)
        .await
    {
        Ok(Some(org)) => org,
        Ok(None) => {
            error!("Organization not found: {}", server.organization);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query organization");
            return;
        }
    };

    let mut local_daemon = match get_local_store(Some(organization.clone())).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to get local store for build");
            update_build_status_recursivly(state, build.clone(), BuildStatus::Aborted).await;
            return;
        }
    };

    let (private_key, public_key) =
        match decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization) {
            Ok(keys) => keys,
            Err(e) => {
                error!(error = %e, "Failed to decrypt SSH private key");
                return;
            }
        };

    let mut server_deamon = connect(
        server.clone(),
        None,
        public_key.clone(),
        private_key.clone(),
    )
    .await;

    for _ in 1..3 {
        if server_deamon.is_ok() {
            break;
        };

        time::sleep(Duration::from_secs(5)).await;
        server_deamon = connect(
            server.clone(),
            None,
            public_key.clone(),
            private_key.clone(),
        )
        .await;
    }

    let mut server_daemon = if let Ok(daemon) = server_deamon {
        daemon
    } else {
        error!("Failed to connect to server after retries");
        requeue_build(state, build.clone()).await;
        return;
    };

    info!("Connected to server successfully");

    let dependencies = match get_build_dependencies_sorted(Arc::clone(&state), &mut local_daemon, &build).await {
        Ok(deps) => deps,
        Err(e) => {
            error!(error = %e, "Failed to get build dependencies");
            update_build_status(Arc::clone(&state), build.clone(), BuildStatus::Failed).await;
            return;
        }
    };

    info!(
        dependency_count = dependencies.len(),
        "Copying dependencies in order"
    );

    for (i, dep) in dependencies.iter().enumerate() {
        debug!(index = i, dependency = %dep, "Dependency order");
    }

    if let Err(e) = match local_daemon {
        LocalNixStore::UnixStream(ref mut store) => {
            copy_builds(dependencies.clone(), store, &mut server_daemon, false).await
        }
        LocalNixStore::CommandDuplex(ref mut store) => {
            copy_builds(dependencies.clone(), store, &mut server_daemon, false).await
        }
    } {
        error!(error = %e, "Failed to copy build dependencies");
        update_build_status(Arc::clone(&state), build.clone(), BuildStatus::Failed).await;
        return;
    }

    let derivation = match create_basic_derivation(&build, &mut local_daemon, dependencies, Arc::clone(&state)).await {
        Ok(derivation) => derivation,
        Err(e) => {
            error!(error = %e, "Failed to create basic derivation");
            update_build_status_recursivly(Arc::clone(&state), build.clone(), BuildStatus::Aborted).await;
            return;
        }
    };

    let mut build_outputs: Vec<ABuildOutput> = vec![];

    match execute_build(&build, derivation, &mut server_daemon, Arc::clone(&state)).await {
        Ok((build_returned, result)) => {
            build = build_returned;
            let status = if result.error_msg.is_empty() {
                let build_results = result.built_outputs;
                let copy_results = build_results
                    .values()
                    .map(|realisation| format!("/nix/store/{}", realisation.out_path))
                    .collect::<Vec<String>>();


                if let Err(e) = match local_daemon {
                    LocalNixStore::UnixStream(ref mut store) => {
                        copy_builds(copy_results, &mut server_daemon, store, true).await
                    }
                    LocalNixStore::CommandDuplex(ref mut store) => {
                        copy_builds(copy_results, &mut server_daemon, store, true).await
                    }
                } {
                    error!(error = %e, "Failed to copy build results");
                    update_build_status(Arc::clone(&state), build.clone(), BuildStatus::Failed)
                        .await;
                    return;
                }

                for (build_output_name, realisation) in build_results {
                    let (build_output_hash, build_output_package) =
                        match get_hash_from_path(format!("/nix/store/{}", realisation.out_path)) {
                            Ok(path_info) => path_info,
                            Err(e) => {
                                error!(error = %e, "Failed to get hash from path");
                                continue;
                            }
                        };

                    build_outputs.push(ABuildOutput {
                        id: Set(Uuid::new_v4()),
                        build: Set(build.id),
                        name: Set(build_output_name),
                        output: Set(format!("/nix/store/{}", realisation.out_path)),
                        hash: Set(build_output_hash),
                        package: Set(build_output_package),
                        file_hash: Set(None),
                        file_size: Set(None),
                        is_cached: Set(false),
                        ca: Set(None),
                        created_at: Set(Utc::now().naive_utc()),
                    });
                }

                BuildStatus::Completed
            } else {
                if !result.error_msg.is_empty() {
                    error!(path = %build.derivation_path, error = %result.error_msg, "Build failed");
                }

                BuildStatus::Failed
            };

            let updated_build = if status == BuildStatus::Failed {
                update_build_status_recursivly(Arc::clone(&state), build.clone(), status).await
            } else {
                update_build_status(Arc::clone(&state), build.clone(), status).await
            };
            info!(build_id = %updated_build.id, status = ?updated_build.status, "Build status updated after execution");
            check_evaluation_status(Arc::clone(&state), build.evaluation).await;
        }

        Err(e) => {
            error!(error = %e, "Failed to execute build");
            update_build_status_recursivly(Arc::clone(&state), build.clone(), BuildStatus::Failed)
                .await;
        }
    };

    if !build_outputs.is_empty() {
        // Insert build outputs in batches to avoid PostgreSQL parameter limits
        const OUTPUT_BATCH_SIZE: usize = 1000;
        for chunk in build_outputs.chunks(OUTPUT_BATCH_SIZE) {
            if let Err(e) = EBuildOutput::insert_many(chunk.to_vec())
                .exec(&state.db)
                .await
            {
                error!(error = %e, "Failed to insert build outputs");
                break;
            }
        }
    }
}

async fn get_next_evaluation(state: Arc<ServerState>) -> MEvaluation {
    loop {
        let threshold_time =
            Utc::now().naive_utc() - chrono::Duration::seconds(state.cli.evaluation_timeout);

        let mut projects = match EProject::find()
            .join(JoinType::InnerJoin, RProject::LastEvaluation.def())
            .filter(
                Condition::all()
                    .add(CProject::Active.eq(true))
                    .add(CProject::LastCheckAt.lte(threshold_time))
                    .add(
                        Condition::any()
                            .add(CEvaluation::Status.eq(EvaluationStatus::Completed))
                            .add(CEvaluation::Status.eq(EvaluationStatus::Failed))
                            .add(CProject::ForceEvaluation.eq(true)),
                    ),
            )
            .order_by_asc(CProject::LastCheckAt)
            .all(&state.db)
            .await
        {
            Ok(projects) => projects,
            Err(e) => {
                error!(error = %e, "Failed to query projects for evaluation");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        match EProject::find()
            .filter(
                Condition::all()
                    .add(CProject::Active.eq(true))
                    .add(CProject::LastCheckAt.lte(threshold_time))
                    .add(CProject::LastEvaluation.is_null()),
            )
            .order_by_asc(CProject::LastCheckAt)
            .all(&state.db)
            .await
        {
            Ok(additional_projects) => projects.extend(additional_projects),
            Err(e) => {
                error!(error = %e, "Failed to query projects without evaluations");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut i_offset = 0;
        for (i, project) in projects.clone().iter().enumerate() {
            let has_no_servers = match EServer::find()
                .filter(
                    Condition::all()
                        .add(CServer::Active.eq(true))
                        .add(CServer::Organization.eq(project.organization)),
                )
                .one(&state.db)
                .await
            {
                Ok(server_opt) => server_opt.is_none(),
                Err(e) => {
                    error!(error = %e, "Failed to query servers for project organization");
                    true // Assume no servers on error
                }
            };

            if has_no_servers {
                projects.remove(i - i_offset);
                i_offset += 1;
            }
        }

        let evaluations = stream::iter(projects.clone().into_iter())
            .filter_map(|p| {
                let intern_state = Arc::clone(&state);
                async move {
                    //TODO: query last evaluation early and pass it to check_project_updates
                    let (has_update, commit_hash) =
                        match check_project_updates(Arc::clone(&intern_state), &p).await {
                            Ok((update, hash)) => (update, hash),
                            Err(e) => {
                                error!(error = %e, "Failed to check project updates");
                                return None;
                            }
                        };

                    if !has_update {
                        return None;
                    }

                    if let Some(evaluation) = p.last_evaluation {
                        match EEvaluation::find_by_id(evaluation)
                            .filter(
                                Condition::any()
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Completed))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Failed))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Aborted)),
                            )
                            .one(&intern_state.db)
                            .await
                        {
                            Ok(Some(eval)) => Some((eval, commit_hash)),
                            Ok(None) => None,
                            Err(_) => None,
                        }
                    } else {
                        Some((
                            MEvaluation {
                                id: Uuid::nil(),
                                project: Some(p.id),
                                repository: p.repository,
                                commit: Uuid::nil(),
                                wildcard: p.evaluation_wildcard,
                                status: EvaluationStatus::Queued,
                                previous: None,
                                next: None,
                                created_at: Utc::now().naive_utc(),
                                error: None,
                            },
                            commit_hash,
                        ))
                    }
                }
            })
            .collect::<Vec<(MEvaluation, Vec<u8>)>>()
            .await;

        if evaluations.is_empty() {
            time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let (evaluation, commit_hash) = match evaluations.first() {
            Some(eval) => eval,
            None => {
                error!("No evaluations found despite non-empty evaluations list");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let project = if let Some(project_id) = evaluation.project {
            projects
                .into_iter()
                .find(|p| p.id == project_id)
                .unwrap_or_else(|| {
                    error!(
                        project_id = %project_id,
                        evaluation_id = %evaluation.id,
                        "Failed to find project for evaluation - critical error"
                    );
                    std::process::exit(1);
                })
        } else {
            // For direct builds, we don't have a project
            error!(
                evaluation_id = %evaluation.id,
                "Direct build evaluation scheduled as regular project evaluation - critical error"
            );
            std::process::exit(1);
        };

        let evaluation_id = if evaluation.id == Uuid::nil() {
            None
        } else {
            Some(evaluation.id)
        };

        let (commit_message, author_email, author_name) =
            match get_commit_info(Arc::clone(&state), &project, commit_hash).await {
                Ok((msg, email, name)) => (msg, email, name),
                Err(e) => {
                    warn!(
                        error = %e,
                        "Failed to fetch commit info, using defaults"
                    );
                    ("".to_string(), None, "".to_string())
                }
            };

        let author_display = if let Some(email) = author_email {
            if !author_name.is_empty() {
                format!("{} <{}>", author_name, email)
            } else {
                email
            }
        } else {
            author_name
        };

        let acommit = ACommit {
            id: Set(Uuid::new_v4()),
            message: Set(commit_message),
            hash: Set(commit_hash.clone()),
            author: Set(None),
            author_name: Set(author_display),
        };

        let commit = match acommit.insert(&state.db).await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "Failed to insert commit");
                continue;
            }
        };
        let new_evaluation = AEvaluation {
            id: Set(Uuid::new_v4()),
            project: Set(Some(project.id)),
            repository: Set(project.repository.clone()),
            commit: Set(commit.id),
            wildcard: Set(project.evaluation_wildcard.clone()),
            status: Set(EvaluationStatus::Queued),
            previous: Set(evaluation_id),
            next: Set(None),
            created_at: Set(Utc::now().naive_utc()),
            error: Set(None),
        };

        let new_evaluation = match new_evaluation.insert(&state.db).await {
            Ok(e) => e,
            Err(e) => {
                error!(error = %e, "Failed to insert evaluation");
                continue;
            }
        };
        info!(evaluation_id = %new_evaluation.id, "Created new evaluation");

        let mut active_project: AProject = project.clone().into();

        active_project.last_check_at = Set(Utc::now().naive_utc());
        active_project.last_evaluation = Set(Some(new_evaluation.id));
        active_project.force_evaluation = Set(false);

        if let Err(e) = active_project.update(&state.db).await {
            error!(error = %e, "Failed to update project");
        }

        if evaluation_id.is_some() {
            let mut active_evaluation: AEvaluation = evaluation.clone().into();
            active_evaluation.next = Set(Some(new_evaluation.id));

            if let Err(e) = active_evaluation.update(&state.db).await {
                error!(error = %e, "Failed to update evaluation");
            }
        };

        return new_evaluation;
    }
}

async fn requeue_build(state: Arc<ServerState>, build: MBuild) -> MBuild {
    let build_entity = match EBuild::find_by_id(build.id).one(&state.db).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            error!(build_id = %build.id, "Build not found for requeueing");
            return build;
        }
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to query build for requeueing");
            return build;
        }
    };

    let mut active_build: ABuild = build_entity.into();
    active_build.status = Set(BuildStatus::Queued);
    active_build.server = Set(None);
    active_build.updated_at = Set(Utc::now().naive_utc());

    match active_build.update(&state.db).await {
        Ok(updated_build) => {
            info!(build_id = %updated_build.id, "Requeueing build");
            updated_build
        }
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to update build for requeueing");
            build
        }
    }
}

async fn get_next_build(state: Arc<ServerState>) -> MBuild {
    loop {
        let builds_sql = sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            r#"
                SELECT * FROM public.build b
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM public.build_dependency d
                    JOIN public.build dep ON d.dependency = dep.id
                    WHERE d.build = b.id AND dep.status != 3
                )
                AND b.status = 1
                ORDER BY b.updated_at ASC
            "#,
        );

        let builds = match EBuild::find().from_raw_sql(builds_sql).all(&state.db).await {
            Ok(builds) => builds,
            Err(e) => {
                error!(error = %e, "Failed to query queued builds");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        debug!(build_count = builds.len(), "Found queued builds");

        for build in builds {
            let evaluation = match EEvaluation::find_by_id(build.evaluation)
                .one(&state.db)
                .await
            {
                Ok(Some(eval)) => eval,
                Ok(None) => {
                    error!(evaluation_id = %build.evaluation, "Evaluation not found for build");
                    continue;
                }
                Err(e) => {
                    error!(error = %e, evaluation_id = %build.evaluation, "Failed to query evaluation for build");
                    continue;
                }
            };

            let project = if let Some(project_id) = evaluation.project {
                match EProject::find_by_id(project_id).one(&state.db).await {
                    Ok(Some(p)) => Some(p),
                    Ok(None) => {
                        error!(project_id = %project_id, "Project not found for evaluation");
                        continue;
                    }
                    Err(e) => {
                        error!(error = %e, project_id = %project_id, "Failed to query project for evaluation");
                        continue;
                    }
                }
            } else {
                None
            };

            let organization_id = if let Some(project) = &project {
                project.organization
            } else {
                // Direct build - get organization from DirectBuild record
                match EDirectBuild::find()
                    .filter(CDirectBuild::Evaluation.eq(evaluation.id))
                    .one(&state.db)
                    .await
                {
                    Ok(Some(direct_build)) => direct_build.organization,
                    Ok(None) => {
                        error!(evaluation_id = %evaluation.id, "Direct build not found for evaluation");
                        continue;
                    }
                    Err(e) => {
                        error!(error = %e, evaluation_id = %evaluation.id, "Failed to query direct build for evaluation");
                        continue;
                    }
                }
            };

            let has_servers = match EServer::find()
                .filter(
                    Condition::all()
                        .add(CServer::Active.eq(true))
                        .add(CServer::Organization.eq(organization_id)),
                )
                .one(&state.db)
                .await
            {
                Ok(server_opt) => server_opt.is_some(),
                Err(e) => {
                    error!(error = %e, "Failed to query servers for organization");
                    false // Assume no servers on error
                }
            };

            if !has_servers {
                // For direct builds, allow local execution instead of aborting
                if evaluation.project.is_none() {
                    debug!(build_id = %build.id, "No servers available, but this is a direct build - will try local execution");
                    return build; // Return for local execution
                } else {
                    update_build_status_recursivly(Arc::clone(&state), build, BuildStatus::Aborted)
                        .await;
                    continue;
                }
            }

            let raw_deps = match EBuildDependency::find()
                .filter(CBuildDependency::Build.eq(build.id))
                .all(&state.db)
                .await
            {
                Ok(deps) => deps,
                Err(e) => {
                    error!(error = %e, build_id = %build.id, "Failed to query raw dependencies for debug");
                    continue;
                }
            };

            debug!(
                build_id = %build.id,
                derivation_path = %build.derivation_path,
                raw_dependency_count = raw_deps.len(),
                "Raw dependency records"
            );

            for dep in &raw_deps {
                debug!(build = %dep.build, dependency = %dep.dependency, "Raw dependency");
            }

            let dependencies = match get_build_dependencies(Arc::clone(&state), &build).await {
                Ok(deps) => deps,
                Err(_) => {
                    error!(build_id = %build.id, "Failed to get dependencies for debug");
                    continue;
                }
            };

            debug!(
                build_id = %build.id,
                derivation_path = %build.derivation_path,
                resolved_dependency_count = dependencies.len(),
                "Resolved dependencies"
            );

            for dep in &dependencies {
                debug!(
                    dependency_id = %dep.id,
                    derivation_path = %dep.derivation_path,
                    status = ?dep.status,
                    "Dependency status"
                );
            }

            if dependencies.is_empty() {
                debug!("No dependencies found - build ready to execute");
            } else {
                let completed_deps = dependencies
                    .iter()
                    .filter(|d| d.status == BuildStatus::Completed)
                    .count();
                debug!(
                    completed = completed_deps,
                    total = dependencies.len(),
                    "Dependency completion status"
                );
            }

            return build;
        }

        time::sleep(Duration::from_secs(5)).await;
    }
}

async fn reserve_available_server(
    state: Arc<ServerState>,
    build: &MBuild,
) -> Option<(MBuild, MServer)> {
    let features = match EBuildFeature::find()
        .filter(CBuildFeature::Build.eq(build.id))
        .all(&state.db)
        .await
    {
        Ok(features) => features
            .into_iter()
            .map(|f| f.feature)
            .collect::<Vec<Uuid>>(),
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to query build features");
            Vec::new()
        }
    };

    let evaluation = match EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await
    {
        Ok(Some(eval)) => eval,
        Ok(None) => {
            error!(evaluation_id = %build.evaluation, "Evaluation not found for build reservation");
            return None;
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %build.evaluation, "Failed to query evaluation for build reservation");
            return None;
        }
    };

    let organization_id = if let Some(project_id) = evaluation.project {
        match EProject::find_by_id(project_id).one(&state.db).await {
            Ok(Some(project)) => project.organization,
            Ok(None) => {
                error!(project_id = %project_id, "Project not found for build reservation");
                return None;
            }
            Err(e) => {
                error!(error = %e, project_id = %project_id, "Failed to query project for build reservation");
                return None;
            }
        }
    } else {
        match EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
        {
            Ok(Some(direct_build)) => direct_build.organization,
            Ok(None) => {
                error!(evaluation_id = %evaluation.id, "Direct build not found for build reservation");
                return None;
            }
            Err(e) => {
                error!(error = %e, evaluation_id = %evaluation.id, "Failed to query direct build for build reservation");
                return None;
            }
        }
    };

    let cond = Condition::all()
        .add(CServer::Organization.eq(organization_id))
        .add(CServer::Active.eq(true));

    let mut cond = if build.architecture != Architecture::BUILTIN {
        cond
            .add(CServerArchitecture::Architecture.eq(build.architecture.clone()))
    } else {
        cond
    };


    for feature in features {
        cond = cond.add(CServerFeature::Feature.eq(feature));
    }

    let servers = match EServer::find()
        .join_rev(
            JoinType::InnerJoin,
            EServerFeature::belongs_to(entity::server::Entity)
                .from(CServerFeature::Server)
                .to(CServer::Id)
                .into(),
        )
        .join_rev(
            JoinType::InnerJoin,
            EServerArchitecture::belongs_to(entity::server::Entity)
                .from(CServerArchitecture::Server)
                .to(CServer::Id)
                .into(),
        )
        .filter(cond)
        .all(&state.db)
        .await
    {
        Ok(servers) => servers,
        Err(e) => {
            error!(error = %e, "Failed to query servers for build reservation");
            return None;
        }
    };

    if servers.is_empty() {
        let build =
            update_build_status_recursivly(state.clone(), build.clone(), BuildStatus::Aborted)
                .await;
        warn!(build_id = %build.id, "Aborted build - no servers found");

        // Update evaluation with error message about no servers
        let evaluation = match EEvaluation::find_by_id(build.evaluation)
            .one(&state.db)
            .await
        {
            Ok(Some(eval)) => eval,
            Ok(None) => {
                error!(evaluation_id = %build.evaluation, "Evaluation not found for server error update");
                return None;
            }
            Err(e) => {
                error!(error = %e, evaluation_id = %build.evaluation, "Failed to query evaluation for server error update");
                return None;
            }
        };

        update_evaluation_status_with_error(
            state,
            evaluation,
            EvaluationStatus::Aborted,
            "No servers available to build this evaluation. Please ensure at least one server is configured and active for the required architecture.".to_string(),
        ).await;

        return None;
    }

    for s in servers {
        let running_builds = match EBuild::find()
            .filter(CBuild::Server.eq(s.id))
            .filter(CBuild::Status.eq(BuildStatus::Building))
            .all(&state.db)
            .await
        {
            Ok(builds) => builds,
            Err(e) => {
                error!(error = %e, server_id = %s.id, "Failed to query running builds for server");
                continue;
            }
        };

        let mut abuild: ABuild = build.clone().into();
        if running_builds.is_empty() {
            abuild.server = Set(Some(s.id));
            abuild.status = Set(BuildStatus::Building);
            abuild.updated_at = Set(Utc::now().naive_utc());

            match abuild.update(&state.db).await {
                Ok(updated_build) => {
                    debug!(build_id = %updated_build.id, "Selected next build");
                    return Some((updated_build, s));
                }
                Err(e) => {
                    error!(error = %e, build_id = %build.id, "Failed to update build for server reservation");
                    continue;
                }
            }
        } else {
            abuild.updated_at = Set(Utc::now().naive_utc());
            if let Err(e) = abuild.update(&state.db).await {
                error!(error = %e, build_id = %build.id, "Failed to update build timestamp");
            }
        }
    }

    None
}

pub async fn update_build_status(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    if status == build.status {
        return build;
    }

    if status == BuildStatus::Aborted
        && (build.status == BuildStatus::Completed || build.status == BuildStatus::Failed)
    {
        return build;
    }

    debug!(build_id = %build.id, status = ?status, "Updating build status");

    let mut active_build: ABuild = build.clone().into_active_model();

    active_build.status = Set(status);
    active_build.updated_at = Set(Utc::now().naive_utc());

    match active_build.update(&state.db).await {
        Ok(updated_build) => updated_build,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to update build status");
            build
        }
    }
}

async fn update_build_status_recursivly(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    use std::collections::{HashSet, VecDeque};

    let mut queue = VecDeque::new();
    let mut processed = HashSet::new();
    queue.push_back(build.id);

    while let Some(current_build_id) = queue.pop_front() {
        if processed.contains(&current_build_id) {
            continue;
        }
        processed.insert(current_build_id);

        let dependencies = match EBuildDependency::find()
            .filter(CBuildDependency::Dependency.eq(current_build_id))
            .all(&state.db)
            .await
        {
            Ok(deps) => deps.into_iter().map(|d| d.build).collect::<Vec<Uuid>>(),
            Err(e) => {
                error!(error = %e, build_id = %current_build_id, "Failed to query build dependencies for update");
                continue;
            }
        };

        if dependencies.is_empty() {
            continue;
        }

        let mut condition = Condition::any();
        for dependency in &dependencies {
            condition = condition.add(CBuild::Id.eq(*dependency));
        }

        let status_condition = if status == BuildStatus::Aborted || status == BuildStatus::Failed {
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building))
        } else {
            Condition::all().add(CBuild::Status.ne(status.clone()))
        };

        let dependent_builds = match EBuild::find()
            .filter(condition)
            .filter(status_condition)
            .all(&state.db)
            .await
        {
            Ok(builds) => builds,
            Err(e) => {
                error!(error = %e, "Failed to query dependent builds for update");
                continue;
            }
        };

        // Update dependent builds and add them to the queue for further processing
        for dependent_build in dependent_builds {
            update_build_status(Arc::clone(&state), dependent_build.clone(), status.clone()).await;
            queue.push_back(dependent_build.id);
        }
    }

    // Finally update the original build
    let build = update_build_status(Arc::clone(&state), build, status.clone()).await;
    check_evaluation_status(state, build.evaluation).await;

    build
}

pub async fn update_evaluation_status(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) -> MEvaluation {
    if status == evaluation.status {
        return evaluation;
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, "Updating evaluation status");

    let mut active_evaluation: AEvaluation = evaluation.clone().into_active_model();
    active_evaluation.status = Set(status);

    match active_evaluation.update(&state.db).await {
        Ok(updated_eval) => updated_eval,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to update evaluation status");
            evaluation
        }
    }
}

pub async fn update_evaluation_status_with_error(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    error_message: String,
) -> MEvaluation {
    if status == evaluation.status && evaluation.error.as_ref() == Some(&error_message) {
        return evaluation;
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, error = %error_message, "Updating evaluation status with error");

    let mut active_evaluation: AEvaluation = evaluation.clone().into_active_model();
    active_evaluation.status = Set(status);
    active_evaluation.error = Set(Some(error_message));

    match active_evaluation.update(&state.db).await {
        Ok(updated_eval) => updated_eval,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to update evaluation status with error");
            evaluation
        }
    }
}

pub async fn abort_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    if evaluation.status == EvaluationStatus::Completed {
        return;
    }

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .filter(
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building)),
        )
        .all(&state.db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to query builds for evaluation abort");
            return;
        }
    };

    for build in builds {
        update_build_status(Arc::clone(&state), build, BuildStatus::Aborted).await;
    }

    update_evaluation_status(state, evaluation, EvaluationStatus::Aborted).await;
}

async fn check_evaluation_status(state: Arc<ServerState>, evaluation_id: Uuid) {
    let evaluation = match EEvaluation::find_by_id(evaluation_id).one(&state.db).await {
        Ok(Some(eval)) => eval,
        Ok(None) => {
            error!(evaluation_id = %evaluation_id, "Evaluation not found for status check");
            return;
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation_id, "Failed to query evaluation for status check");
            return;
        }
    };

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .all(&state.db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation_id, "Failed to query builds for evaluation status check");
            return;
        }
    };

    let statuses = builds
        .into_iter()
        .map(|b| b.status)
        .collect::<Vec<BuildStatus>>();

    let status = if statuses.iter().all(|s| *s == BuildStatus::Completed) {
        EvaluationStatus::Completed
    } else if statuses.contains(&BuildStatus::Building) {
        EvaluationStatus::Failed
    } else if statuses.contains(&BuildStatus::Aborted) {
        EvaluationStatus::Aborted
    } else if statuses.contains(&BuildStatus::Failed) {
        EvaluationStatus::Failed
    } else {
        return;
    };

    update_evaluation_status(state, evaluation, status).await;
}

async fn get_build_dependencies(
    state: Arc<ServerState>,
    build: &MBuild,
) -> Result<Vec<MBuild>, String> {
    let dependencies = match EBuildDependency::find()
        .filter(CBuildDependency::Build.eq(build.id))
        .all(&state.db)
        .await
    {
        Ok(deps) => deps
            .into_iter()
            .map(|d| d.dependency)
            .collect::<Vec<Uuid>>(),
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to query build dependencies");
            return Err("Failed to query build dependencies".to_string());
        }
    };

    let mut condition = Condition::any();

    for dependency in dependencies {
        condition = condition.add(CBuild::Id.eq(dependency));
    }

    let builds = match EBuild::find().filter(condition).all(&state.db).await {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, "Failed to query builds for dependencies");
            return Err("Failed to query builds for dependencies".to_string());
        }
    };

    Ok(builds)
}

async fn get_build_dependencies_sorted(
    state: Arc<ServerState>,
    local_store: &mut LocalNixStore,
    build: &MBuild,
) -> Result<Vec<String>, String> {
    let bdependencies_direct: Vec<MBuild> = match get_build_dependencies(Arc::clone(&state), build).await {
        Ok(deps) => deps,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to get build dependencies for sorting");
            return Err(e);
        }
    };

    let mut dependencies = HashSet::new();
    for dependency in &bdependencies_direct {
        let output_map = if dependency.derivation_path.ends_with(".drv") {
            // TODO: find better way to get correct dependencies
            let mut deps = match local_store {
                LocalNixStore::UnixStream(store) => {
                    get_output_paths(dependency.derivation_path.clone(), store).await
                }
                LocalNixStore::CommandDuplex(store) => {
                    get_output_paths(dependency.derivation_path.clone(), store).await
                }
            }.map_err(|e| {
                error!(error = %e, derivation_path = %dependency.derivation_path, "Failed to get output path for dependency");
                "Failed to get output path for dependency".to_string()
            })?
            .values().cloned().collect::<Vec<String>>();

            let missing = match local_store {
                LocalNixStore::UnixStream(store) => {
                    get_missing_builds(deps.clone(), store).await
                } LocalNixStore::CommandDuplex(store) => {
                    get_missing_builds(deps.clone(), store).await
                }
            }.map_err(|e| {
                error!(error = %e, derivation_path = %dependency.derivation_path, "Failed to get missing builds for dependency");
                "Failed to get missing builds for dependency".to_string()
            })?;

            deps.retain(|d| !missing.contains(d));
            deps
        } else {
            vec![dependency.derivation_path.clone()]
        };

        dependencies.extend(output_map);
    }

    Ok(dependencies.into_iter().collect())
}
