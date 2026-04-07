/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;
use chrono::Utc;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::server::Architecture;
use gradient_core::executer::*;
use gradient_core::types::*;
use nix_daemon::{BasicDerivation, DerivationOutput};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::status::{
    check_evaluation_status, update_build_status, update_build_status_recursivly,
    update_evaluation_status,
};

type OutputInfo = HashMap<String, (Option<String>, Option<String>, Option<String>)>;

/// Parses a `.drv` file directly and extracts the builder, args, env, input sources,
/// and per-output path/hash info needed to construct a `BasicDerivation`.
async fn parse_derivation_file(
    state: &Arc<ServerState>,
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

    let drv = state
        .derivation_resolver
        .get_derivation(derivation_path.to_string())
        .await
        .with_context(|| format!("Failed to parse derivation file: {}", derivation_path))?;

    let output_info: OutputInfo = drv
        .outputs
        .into_iter()
        .map(|o| {
            let path = if o.path.is_empty() {
                None
            } else {
                Some(o.path)
            };
            let hash_algo = if o.hash_algo.is_empty() {
                None
            } else {
                Some(o.hash_algo)
            };
            let hash = if o.hash.is_empty() {
                None
            } else {
                Some(o.hash)
            };
            (o.name, (path, hash_algo, hash))
        })
        .collect();

    let input_srcs: HashSet<String> = drv.input_sources.into_iter().collect();

    Ok((
        drv.builder,
        drv.args,
        drv.environment,
        input_srcs,
        output_info,
    ))
}

/// Constructs a `BasicDerivation` for submission to the remote Nix daemon by combining
/// parsed derivation data with the resolved dependency output paths.
async fn create_basic_derivation(
    state: &Arc<ServerState>,
    build: &MBuild,
    _local_daemon: &mut LocalNixStore,
    dependencies: Vec<String>,
) -> anyhow::Result<BasicDerivation> {
    let (builder, args, env, input_srcs, output_info) =
        parse_derivation_file(state, &build.derivation_path)
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
            } else {
                break;
            }
        }

        // After each cycle, reconcile Waiting ↔ Building based on server availability.
        update_waiting_evaluations(Arc::clone(&state)).await;

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
        "Resolved build dependencies"
    );

    for (i, dep) in dependencies.iter().enumerate() {
        debug!(index = i, dependency = %dep, "Dependency order");
    }

    let derivation = match create_basic_derivation(
        &state,
        &build,
        &mut local_daemon,
        dependencies.clone(),
    )
    .await
    {
        Ok(derivation) => derivation,
        Err(e) => {
            error!(error = %e, "Failed to create basic derivation");
            update_build_status_recursivly(Arc::clone(&state), build.clone(), BuildStatus::Aborted)
                .await;
            return;
        }
    };

    // Drop the local daemon before calling the executor: the SSH executor
    // will acquire its own local store connection internally.
    drop(local_daemon);

    let mut build_outputs: Vec<ABuildOutput> = vec![];

    let exec_result = state
        .build_executor
        .execute(
            Arc::clone(&state),
            server.clone(),
            organization.clone(),
            build.clone(),
            derivation,
            dependencies,
        )
        .await;

    match exec_result {
        Ok(result) => {
            // Successful connection happened inside `execute`; record it.
            let mut aserver: AServer = server.clone().into();
            aserver.last_connection_at = Set(Utc::now().naive_utc());
            if let Err(e) = aserver.update(&state.db).await {
                warn!(error = %e, "Failed to update server last_connection_at");
            }

            let status = if result.error_msg.is_empty() {
                for output in result.outputs {
                    build_outputs.push(ABuildOutput {
                        id: Set(Uuid::new_v4()),
                        build: Set(build.id),
                        name: Set(output.name),
                        output: Set(output.store_path),
                        hash: Set(output.hash),
                        package: Set(output.package),
                        file_hash: Set(None),
                        file_size: Set(output.nar_size),
                        is_cached: Set(false),
                        has_artefacts: Set(output.has_artefacts),
                        ca: Set(None),
                        created_at: Set(Utc::now().naive_utc()),
                        last_fetched_at: Set(None),
                    });
                }
                BuildStatus::Completed
            } else {
                error!(path = %build.derivation_path, error = %result.error_msg, "Build failed");
                BuildStatus::Failed
            };

            if status == BuildStatus::Completed {
                build.build_time_ms = Some(result.elapsed.as_millis() as i64);
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
            error!(error = %e, "Build executor failed");
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

/// Waits for the next build that has all dependencies completed and returns it.
///
/// Uses a raw SQL query to efficiently find builds whose dependency graph is fully satisfied
/// (no non-Completed dependencies). Loops until a suitable build is found.
async fn get_next_build(state: Arc<ServerState>) -> MBuild {
    loop {
        // Order ready-to-run builds by direct dependency count (descending)
        // so that integration builds — the ones that pulled in the largest
        // amount of work to become ready — start first. Tie-break by
        // `updated_at ASC` so older builds still drain before newer ones.
        let builds_sql = sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            r#"
                SELECT b.*
                FROM public.build b
                LEFT JOIN public.build_dependency bd ON bd.build = b.id
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM public.build_dependency d
                    JOIN public.build dep ON d.dependency = dep.id
                    WHERE d.build = b.id AND dep.status != 3
                )
                AND b.status = 1
                GROUP BY b.id
                ORDER BY COUNT(bd.dependency) DESC, b.updated_at ASC
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
        // No server matches the required arch/features — leave the build Queued
        // so the scheduler retries. The evaluation will be moved to Waiting by
        // `update_waiting_evaluations` once all schedulable builds have run.
        debug!(
            build_id = %build.id,
            derivation = %build.derivation_path,
            "No matching server available, leaving build queued"
        );
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

/// Reconciles `Building ↔ Waiting` evaluation states each scheduler tick.
///
/// - `Building` evaluation with queued builds but no active servers in the org → `Waiting`
/// - `Waiting` evaluation whose org now has at least one active server → `Building`
async fn update_waiting_evaluations(state: Arc<ServerState>) {
    let evals = match EEvaluation::find()
        .filter(
            Condition::any()
                .add(CEvaluation::Status.eq(EvaluationStatus::Building))
                .add(CEvaluation::Status.eq(EvaluationStatus::Waiting)),
        )
        .all(&state.db)
        .await
    {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "Failed to query evaluations for waiting reconciliation");
            return;
        }
    };

    for eval in evals {
        let org_id = if let Some(project_id) = eval.project {
            match EProject::find_by_id(project_id).one(&state.db).await {
                Ok(Some(p)) => p.organization,
                Ok(None) => continue,
                Err(e) => {
                    warn!(error = %e, evaluation_id = %eval.id, "Failed to query project for waiting check");
                    continue;
                }
            }
        } else {
            match EDirectBuild::find()
                .filter(CDirectBuild::Evaluation.eq(eval.id))
                .one(&state.db)
                .await
            {
                Ok(Some(db)) => db.organization,
                Ok(None) => continue,
                Err(e) => {
                    warn!(error = %e, evaluation_id = %eval.id, "Failed to query direct build for waiting check");
                    continue;
                }
            }
        };

        let has_active_servers = match EServer::find()
            .filter(
                Condition::all()
                    .add(CServer::Organization.eq(org_id))
                    .add(CServer::Active.eq(true)),
            )
            .one(&state.db)
            .await
        {
            Ok(s) => s.is_some(),
            Err(e) => {
                warn!(error = %e, evaluation_id = %eval.id, "Failed to query servers for waiting check");
                continue;
            }
        };

        if eval.status == EvaluationStatus::Building && !has_active_servers {
            let has_queued = match EBuild::find()
                .filter(CBuild::Evaluation.eq(eval.id))
                .filter(CBuild::Status.eq(BuildStatus::Queued))
                .one(&state.db)
                .await
            {
                Ok(b) => b.is_some(),
                Err(e) => {
                    warn!(error = %e, evaluation_id = %eval.id, "Failed to query queued builds for waiting check");
                    continue;
                }
            };

            if has_queued {
                info!(evaluation_id = %eval.id, "No active servers in org, moving evaluation to Waiting");
                update_evaluation_status(Arc::clone(&state), eval, EvaluationStatus::Waiting).await;
            }
        } else if eval.status == EvaluationStatus::Waiting && has_active_servers {
            info!(evaluation_id = %eval.id, "Servers now available, resuming evaluation to Building");
            update_evaluation_status(Arc::clone(&state), eval, EvaluationStatus::Building).await;
        }
    }
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

            let missing = state.nix_store.query_missing_paths(deps.clone()).await
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
