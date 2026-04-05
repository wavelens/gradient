/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;
use chrono::Utc;
use core::executer::*;
use core::sources::*;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::server::Architecture;
use nix_daemon::{BasicDerivation, DerivationOutput};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter,
    QuerySelect,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::status::{
    check_evaluation_status, update_build_status, update_build_status_recursivly,
    update_evaluation_status_with_error,
};

type OutputInfo = HashMap<String, (Option<String>, Option<String>, Option<String>)>;

/// Parses a `.drv` file via `nix derivation show` and extracts the builder, args, env,
/// input sources, and per-output path/hash info needed to construct a `BasicDerivation`.
async fn parse_derivation_file(
    binpath_nix: &str,
    derivation_path: &str,
) -> anyhow::Result<(
    String,
    Vec<String>,
    HashMap<String, String>,
    HashSet<String>,
    OutputInfo,
)> {
    if !derivation_path.ends_with(".drv") {
        return Ok((
            "/bin/bash".to_string(),
            vec![],
            HashMap::new(),
            HashSet::new(),
            HashMap::new(),
        ));
    }

    let full_drv_path = nix_store_path(derivation_path);
    debug!(cmd = %format!("{} derivation show {}", binpath_nix, full_drv_path), "executing nix command");
    let output = Command::new(binpath_nix)
        .arg("derivation")
        .arg("show")
        .arg(&full_drv_path)
        .output()
        .await
        .context("Failed to execute nix derivation show command")?;

    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }

    let json_output = String::from_utf8_lossy(&output.stdout);
    let parsed_json: serde_json::Value = serde_json::from_str(&json_output).with_context(|| {
        format!(
            "Failed to parse JSON output from 'nix derivation show {}': '{}', stderr: '{}'",
            derivation_path,
            json_output,
            String::from_utf8_lossy(&output.stderr)
        )
    })?;

    let top = parsed_json
        .as_object()
        .context("nix derivation show: expected top-level JSON object")?;
    let drv_map = if let Some(inner) = top.get("derivations").and_then(|v| v.as_object()) {
        inner
    } else {
        top
    };
    let derivation_data = drv_map
        .values()
        .next()
        .context("nix derivation show: output object was empty")?
        .as_object()
        .context("nix derivation show: derivation entry is not an object")?;

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
                .map(|(k, s)| (k.to_string(), s.to_string()))
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

    // Extract per-output info: path, hashAlgo, and hash.
    //
    // New nix JSON format (2.22+):
    //   - "path" omits the "/nix/store/" prefix (or is absent for FODs)
    //   - "method": "flat" | "nar" | "text"  (separate from hashAlgo)
    //   - "hashAlgo": "sha256"  (just the algorithm)
    //
    // Old nix JSON format:
    //   - "path" includes the full "/nix/store/..." path
    //   - "hashAlgo": "r:sha256" | "sha256" | "text:sha256"  (method+algo combined)
    //   - no "method" field
    let output_info: OutputInfo = derivation_data
        .get("outputs")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| {
                    // Path: new nix omits the store prefix; old nix includes it.
                    let path = v
                        .get("path")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            if s.starts_with("/nix/store/") {
                                s.to_string()
                            } else {
                                format!("/nix/store/{}", s)
                            }
                        });

                    // hashAlgo: combine method + algo for new nix format.
                    // Old nix already includes the method in hashAlgo ("r:sha256").
                    let algo = v
                        .get("hashAlgo")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());
                    let hash_algo = algo.map(|algo| {
                        // If algo already contains ":" it's old format (method included).
                        if algo.contains(':') {
                            algo.to_string()
                        } else {
                            // New format: look for separate "method" field.
                            match v.get("method").and_then(|v| v.as_str()) {
                                Some("nar") => format!("r:{}", algo),
                                Some("text") => format!("text:{}", algo),
                                _ => algo.to_string(), // "flat" or absent → no prefix
                            }
                        }
                    });

                    let hash = v
                        .get("hash")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());

                    (k.to_string(), (path, hash_algo, hash))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok((builder, args, env, input_srcs, output_info))
}

/// Constructs a `BasicDerivation` for submission to the remote Nix daemon by combining
/// parsed derivation data with the resolved dependency output paths.
async fn create_basic_derivation(
    build: &MBuild,
    _local_daemon: &mut LocalNixStore,
    dependencies: Vec<String>,
    state: Arc<ServerState>,
) -> anyhow::Result<BasicDerivation> {
    let (builder, args, env, input_srcs, output_info) =
        parse_derivation_file(state.cli.binpath_nix.as_str(), &build.derivation_path)
            .await
            .context("Failed to parse derivation file")?;

    // Build outputs from `nix derivation show` JSON. Always include the path.
    let mut outputs = HashMap::new();
    for (name, (json_path, hash_algo, hash)) in output_info {
        outputs.insert(
            name,
            DerivationOutput {
                path: json_path,
                hash_algo,
                hash,
            },
        );
    }

    let input_srcs: HashSet<String> = dependencies
        .into_iter()
        .chain(input_srcs.into_iter())
        .collect();

    Ok(BasicDerivation {
        outputs,
        input_srcs: input_srcs.into_iter().collect(),
        platform: build.architecture.to_string(),
        builder,
        args,
        env, // env now contains __json if structured attrs exist
    })
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
        match decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization, &state.cli.serve_url) {
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

    let mut aserver: AServer = server.clone().into();
    aserver.last_connection_at = Set(Utc::now().naive_utc());
    if let Err(e) = aserver.update(&state.db).await {
        warn!(error = %e, "Failed to update server last_connection_at");
    }

    let dependencies =
        match get_build_dependencies_sorted(Arc::clone(&state), &mut local_daemon, &build).await {
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

    if let Err(e) = copy_builds(dependencies.clone(), &mut local_daemon, &mut server_daemon, false).await {
        error!(error = %e, "Failed to copy build dependencies");
        update_build_status(Arc::clone(&state), build.clone(), BuildStatus::Failed).await;
        return;
    }

    let derivation =
        match create_basic_derivation(&build, &mut local_daemon, dependencies, Arc::clone(&state))
            .await
        {
            Ok(derivation) => derivation,
            Err(e) => {
                error!(error = %e, "Failed to create basic derivation");
                update_build_status_recursivly(
                    Arc::clone(&state),
                    build.clone(),
                    BuildStatus::Aborted,
                )
                .await;
                return;
            }
        };

    let mut build_outputs: Vec<ABuildOutput> = vec![];

    let build_start = Instant::now();

    match execute_build(&build, derivation, &mut server_daemon, Arc::clone(&state)).await {
        Ok((build_returned, result)) => {
            build = build_returned;
            let status = if result.error_msg.is_empty() {
                let build_results = result.built_outputs;
                let copy_results = build_results
                    .values()
                    .map(|realisation| format!("/nix/store/{}", realisation.out_path))
                    .collect::<Vec<String>>();

                if let Err(e) = copy_builds(copy_results, &mut server_daemon, &mut local_daemon, true).await {
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

                    let output_path = format!("/nix/store/{}", realisation.out_path);
                    let has_artefacts = tokio::fs::metadata(
                        format!("{}/nix-support/hydra-build-products", output_path)
                    ).await.is_ok();

                    let file_size = match get_pathinfo(output_path.clone(), &mut local_daemon).await {
                        Ok(Some(info)) => Some(info.nar_size as i64),
                        _ => None,
                    };

                    build_outputs.push(ABuildOutput {
                        id: Set(Uuid::new_v4()),
                        build: Set(build.id),
                        name: Set(build_output_name),
                        output: Set(output_path),
                        hash: Set(build_output_hash),
                        package: Set(build_output_package),
                        file_hash: Set(None),
                        file_size: Set(file_size),
                        is_cached: Set(false),
                        has_artefacts: Set(has_artefacts),
                        ca: Set(None),
                        created_at: Set(Utc::now().naive_utc()),
                        last_fetched_at: Set(None),
                    });
                }

                BuildStatus::Completed
            } else {
                if !result.error_msg.is_empty() {
                    error!(path = %build.derivation_path, error = %result.error_msg, "Build failed");
                }

                BuildStatus::Failed
            };

            if status == BuildStatus::Completed {
                build.build_time_ms = Some(build_start.elapsed().as_millis() as i64);
            }

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

/// Waits for the next build that has all dependencies completed and returns it.
///
/// Uses a raw SQL query to efficiently find builds whose dependency graph is fully satisfied
/// (no non-Completed dependencies). Loops until a suitable build is found.
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

/// Finds an available server for the build's architecture and required features, atomically
/// claims it by setting the build status to `Building`, and returns the (build, server) pair.
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
        cond.add(CServerArchitecture::Architecture.eq(build.architecture.clone()))
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
            format!("No servers available to build {}. Please ensure at least one server is configured and active for the required architecture.", build.derivation_path).to_string(),
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
        if running_builds.len() < s.max_concurrent_builds as usize {
            abuild.server = Set(Some(s.id));
            abuild.status = Set(BuildStatus::Building);
            abuild.updated_at = Set(Utc::now().naive_utc());

            match abuild.update(&state.db).await {
                Ok(updated_build) => {
                    debug!(build_id = %updated_build.id, server_id = %s.id, running = running_builds.len(), max = s.max_concurrent_builds, "Selected next build");
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

/// Returns the direct dependency builds of a build (one level, not recursive).
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

/// Resolves a build's dependencies to a flat list of already-built store paths,
/// filtering out any that are still missing from the local store.
async fn get_build_dependencies_sorted(
    state: Arc<ServerState>,
    local_store: &mut LocalNixStore,
    build: &MBuild,
) -> Result<Vec<String>, String> {
    let bdependencies_direct: Vec<MBuild> = match get_build_dependencies(Arc::clone(&state), build)
        .await
    {
        Ok(deps) => deps,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to get build dependencies for sorting");
            return Err(e);
        }
    };

    let mut dependencies = HashSet::new();
    for dependency in &bdependencies_direct {
        let dep_full_path = nix_store_path(&dependency.derivation_path);
        let output_map = if dependency.derivation_path.ends_with(".drv") {
            // TODO: find better way to get correct dependencies
            let mut deps = get_output_paths(dep_full_path.clone(), local_store).await
            .map_err(|e| {
                error!(error = %e, derivation_path = %dependency.derivation_path, "Failed to get output path for dependency");
                "Failed to get output path for dependency".to_string()
            })?
            .values().cloned().collect::<Vec<String>>();

            let missing = get_missing_builds(deps.clone(), local_store).await
            .map_err(|e| {
                error!(error = %e, derivation_path = %dependency.derivation_path, "Failed to get missing builds for dependency");
                "Failed to get missing builds for dependency".to_string()
            })?;

            deps.retain(|d| !missing.contains(d));
            deps
        } else {
            vec![dep_full_path]
        };

        dependencies.extend(output_map);
    }

    Ok(dependencies.into_iter().collect())
}
