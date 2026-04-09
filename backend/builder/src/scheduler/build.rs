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
use bytes::Bytes;
use gradient_core::executer::*;
use gradient_core::types::*;
use harmonia_store_core::derivation::{BasicDerivation, DerivationOutput, DerivationT};
use harmonia_store_core::store_path::StorePath;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    derivation: &MDerivation,
    dependencies: Vec<String>,
) -> anyhow::Result<BasicDerivation> {
    let (builder, args, env, input_srcs, output_info) =
        parse_derivation_file(state, &derivation.derivation_path)
            .await
            .context("Failed to parse derivation file")?;

    // Build harmonia DerivationOutputs from parsed output info.
    let mut outputs = BTreeMap::new();
    for (name, (json_path, _hash_algo, _hash)) in output_info {
        let output_name = name
            .parse()
            .with_context(|| format!("Invalid output name: {}", name))?;

        let drv_output = if let Some(path) = json_path {
            let sp = StorePath::from_base_path(&strip_nix_store_prefix(&path))
                .with_context(|| format!("Invalid output store path: {}", path))?;
            DerivationOutput::InputAddressed(sp)
        } else {
            DerivationOutput::Deferred
        };

        outputs.insert(output_name, drv_output);
    }

    // Combine explicit dependencies with input sources.
    let inputs = dependencies
        .into_iter()
        .chain(input_srcs.into_iter())
        .filter_map(|p| {
            let base = strip_nix_store_prefix(&nix_store_path(&p));
            StorePath::from_base_path(&base).ok()
        })
        .collect();

    // Extract derivation name from the store path (e.g. "hash-name.drv" → "name.drv").
    let drv_base = strip_nix_store_prefix(&derivation.derivation_path);
    let drv_name = drv_base
        .find('-')
        .map(|i| &drv_base[i + 1..])
        .unwrap_or(&drv_base);

    Ok(DerivationT {
        name: drv_name
            .parse()
            .with_context(|| format!("Invalid derivation name: {}", drv_name))?,
        outputs,
        inputs,
        platform: Bytes::from(derivation.architecture.to_string()),
        builder: Bytes::from(builder),
        args: args.into_iter().map(Bytes::from).collect(),
        env: env
            .into_iter()
            .map(|(k, v)| (Bytes::from(k), Bytes::from(v)))
            .collect(),
        structured_attrs: None,
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
        let mut skipped: HashSet<Uuid> = HashSet::new();
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        while current_schedules.len() < state.cli.max_concurrent_builds {
            let (build, derivation) = match get_next_build(Arc::clone(&state), &skipped).await {
                Some(b) => b,
                None => break,
            };
            debug!(build_id = %build.id, derivation = %derivation.derivation_path, "Processing build from queue");

            if let Some((build, server)) =
                reserve_available_server(Arc::clone(&state), &build, &derivation).await
            {
                info!(server_id = %server.id, build_id = %build.id, "Reserving server for build");
                let schedule = tokio::spawn(schedule_build(
                    Arc::clone(&state),
                    build,
                    derivation.clone(),
                    server,
                ));
                current_schedules.push(schedule);
                added_schedule = true;
            } else {
                // No server matched / claim failed — try the next queued build
                // this tick instead of blocking the head of the queue.
                skipped.insert(build.id);
                continue;
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

#[instrument(skip(state, derivation), fields(build_id = %build.id, server_id = %server.id, derivation_path = %derivation.derivation_path))]
pub async fn schedule_build(
    state: Arc<ServerState>,
    mut build: MBuild,
    derivation: MDerivation,
    server: MServer,
) {
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

    let basic_derivation =
        match create_basic_derivation(&state, &derivation, dependencies.clone())
            .await
        {
            Ok(d) => d,
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

    // Drop the local daemon before calling the executor: the SSH executor
    // will acquire its own local store connection internally.
    drop(local_daemon);

    let exec_result = state
        .build_executor
        .execute(
            Arc::clone(&state),
            server.clone(),
            organization.clone(),
            build.clone(),
            derivation.derivation_path.clone(),
            basic_derivation,
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
                // Update the already-existing derivation_output rows with
                // the sizes/hashes we just observed. Insert any rows that
                // were discovered for the first time on this build.
                for output in result.outputs {
                    let existing = EDerivationOutput::find()
                        .filter(CDerivationOutput::Derivation.eq(derivation.id))
                        .filter(CDerivationOutput::Name.eq(output.name.clone()))
                        .one(&state.db)
                        .await
                        .unwrap_or(None);

                    if let Some(row) = existing {
                        let mut a: ADerivationOutput = row.into();
                        a.output = Set(output.store_path);
                        a.hash = Set(output.hash);
                        a.package = Set(output.package);
                        a.file_size = Set(output.nar_size);
                        a.has_artefacts = Set(output.has_artefacts);
                        if let Err(e) = a.update(&state.db).await {
                            error!(error = %e, "Failed to update derivation_output after build");
                        }
                    } else if let Err(e) = (ADerivationOutput {
                        id: Set(Uuid::new_v4()),
                        derivation: Set(derivation.id),
                        name: Set(output.name),
                        output: Set(output.store_path),
                        hash: Set(output.hash),
                        package: Set(output.package),
                        ca: Set(None),
                        file_hash: Set(None),
                        file_size: Set(output.nar_size),
                        nar_size: Set(None),
                        is_cached: Set(false),
                        has_artefacts: Set(output.has_artefacts),
                        created_at: Set(Utc::now().naive_utc()),
                    })
                    .insert(&state.db)
                    .await
                    {
                        error!(error = %e, "Failed to insert derivation_output after build");
                    }
                }
                BuildStatus::Completed
            } else {
                error!(path = %derivation.derivation_path, error = %result.error_msg, "Build failed");
                BuildStatus::Failed
            };

            if status == BuildStatus::Completed {
                // Persist the build_time_ms before update_build_status runs.
                // `into_active_model()` marks the mutated field as `Unchanged`,
                // so mutating the Model in place would not reach the DB.
                let mut a = build.clone().into_active_model();
                a.build_time_ms = Set(Some(result.elapsed.as_millis() as i64));
                match a.update(&state.db).await {
                    Ok(updated) => build = updated,
                    Err(e) => {
                        error!(error = %e, build_id = %build.id, "Failed to persist build_time_ms");
                    }
                }
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
            error!(error = format!("{:#}", e), "Build executor failed");
            update_build_status_recursivly(Arc::clone(&state), build.clone(), BuildStatus::Failed)
                .await;
        }
    };
}

/// Returns the next ready-to-run build that is not in `skip`, or `None` if none is available.
///
/// Uses a raw SQL query to efficiently find builds whose dependency graph is fully satisfied
/// (no non-Completed dependencies). Returns `None` immediately if nothing is schedulable —
/// the outer scheduler loop owns the retry interval.
async fn get_next_build(
    state: Arc<ServerState>,
    skip: &HashSet<Uuid>,
) -> Option<(MBuild, MDerivation)> {
    // A build is ready when every derivation_dependency edge of its
    // derivation resolves, in the same evaluation, to a build whose
    // status is Completed or Substituted (status 3 or 7).
    //
    // Order ready-to-run builds by direct dependency count (descending)
    // so that integration builds — the ones that pulled in the largest
    // amount of work to become ready — start first. Tie-break by
    // `updated_at ASC` so older builds still drain before newer ones.
    let builds_sql = sea_orm::Statement::from_string(
        sea_orm::DbBackend::Postgres,
        r#"
                SELECT b.*
                FROM public.build b
                LEFT JOIN public.derivation_dependency dd
                    ON dd.derivation = b.derivation
                WHERE b.status = 1
                AND NOT EXISTS (
                    SELECT 1
                    FROM public.derivation_dependency dep_edge
                    LEFT JOIN public.build dep_build
                        ON dep_build.derivation = dep_edge.dependency
                        AND dep_build.evaluation = b.evaluation
                    WHERE dep_edge.derivation = b.derivation
                        AND (dep_build.id IS NULL
                            OR (dep_build.status != 3 AND dep_build.status != 7))
                )
                GROUP BY b.id
                ORDER BY COUNT(dd.dependency) DESC, b.updated_at ASC
            "#,
    );

    let builds = match EBuild::find().from_raw_sql(builds_sql).all(&state.db).await {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, "Failed to query queued builds");
            return None;
        }
    };

    debug!(build_count = builds.len(), "Found queued builds");

    for build in builds {
        if skip.contains(&build.id) {
            continue;
        }
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

        // Load the joined derivation row.
        let derivation = match EDerivation::find_by_id(build.derivation)
            .one(&state.db)
            .await
        {
            Ok(Some(d)) => d,
            Ok(None) => {
                error!(build_id = %build.id, "Derivation not found for build");
                continue;
            }
            Err(e) => {
                error!(error = %e, build_id = %build.id, "Failed to query derivation for build");
                continue;
            }
        };

        if !has_servers {
            // For direct builds, allow local execution instead of aborting
            if evaluation.project.is_none() {
                debug!(build_id = %build.id, "No servers available, but this is a direct build - will try local execution");
                return Some((build, derivation));
            } else {
                // No active servers in org — leave the build queued; the
                // evaluation will be reconciled to Waiting by
                // `update_waiting_evaluations`.
                continue;
            }
        }

        return Some((build, derivation));
    }

    None
}

/// Finds an available server for the build's architecture and required features, atomically
/// claims it by setting the build status to `Building`, and returns the (build, server) pair.
async fn reserve_available_server(
    state: Arc<ServerState>,
    build: &MBuild,
    derivation: &MDerivation,
) -> Option<(MBuild, MServer)> {
    let features = match EDerivationFeature::find()
        .filter(CDerivationFeature::Derivation.eq(derivation.id))
        .all(&state.db)
        .await
    {
        Ok(features) => features
            .into_iter()
            .map(|f| f.feature)
            .collect::<Vec<Uuid>>(),
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to query derivation features");
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

    // Step 1: candidate servers — active + in the right org.
    let candidate_servers = match EServer::find()
        .filter(
            Condition::all()
                .add(CServer::Organization.eq(organization_id))
                .add(CServer::Active.eq(true)),
        )
        .all(&state.db)
        .await
    {
        Ok(servers) => servers,
        Err(e) => {
            error!(error = %e, "Failed to query servers for build reservation");
            return None;
        }
    };

    // Step 2: filter by architecture (skipped for BUILTIN builds).
    let arch_ok_ids: HashSet<Uuid> =
        if derivation.architecture != Architecture::BUILTIN && !candidate_servers.is_empty() {
            let mut id_cond = Condition::any();
            for s in &candidate_servers {
                id_cond = id_cond.add(CServerArchitecture::Server.eq(s.id));
            }
            match EServerArchitecture::find()
                .filter(
                    Condition::all()
                        .add(id_cond)
                        .add(CServerArchitecture::Architecture.eq(derivation.architecture.clone())),
                )
                .all(&state.db)
                .await
            {
                Ok(rows) => rows.into_iter().map(|r| r.server).collect(),
                Err(e) => {
                    error!(error = %e, "Failed to query server architectures");
                    return None;
                }
            }
        } else {
            candidate_servers.iter().map(|s| s.id).collect()
        };

    // Step 3: filter by features — server must have ALL required features.
    let feature_ok_ids: HashSet<Uuid> = if features.is_empty() {
        arch_ok_ids.clone()
    } else if arch_ok_ids.is_empty() {
        HashSet::new()
    } else {
        let mut id_cond = Condition::any();
        for id in &arch_ok_ids {
            id_cond = id_cond.add(CServerFeature::Server.eq(*id));
        }
        let mut feat_cond = Condition::any();
        for f in &features {
            feat_cond = feat_cond.add(CServerFeature::Feature.eq(*f));
        }
        let rows = match EServerFeature::find()
            .filter(Condition::all().add(id_cond).add(feat_cond))
            .all(&state.db)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "Failed to query server features");
                return None;
            }
        };
        let mut count_per_server: HashMap<Uuid, usize> = HashMap::new();
        for row in rows {
            *count_per_server.entry(row.server).or_insert(0) += 1;
        }
        count_per_server
            .into_iter()
            .filter(|(_, c)| *c >= features.len())
            .map(|(id, _)| id)
            .collect()
    };

    let servers: Vec<MServer> = candidate_servers
        .into_iter()
        .filter(|s| feature_ok_ids.contains(&s.id))
        .collect();

    if servers.is_empty() {
        // No server matches the required arch/features — leave the build Queued
        // so the scheduler retries. The evaluation will be moved to Waiting by
        // `update_waiting_evaluations` once all schedulable builds have run.
        debug!(
            build_id = %build.id,
            derivation = %derivation.derivation_path,
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

/// Returns `true` if at least one queued build in `eval_id` can be scheduled
/// on some active server in `org_id` (matching architecture + all required
/// features). Returns `false` if there are no queued builds at all.
async fn any_queued_build_schedulable(
    state: &Arc<ServerState>,
    eval_id: Uuid,
    org_id: Uuid,
) -> bool {
    let queued = match EBuild::find()
        .filter(CBuild::Evaluation.eq(eval_id))
        .filter(CBuild::Status.eq(BuildStatus::Queued))
        .all(&state.db)
        .await
    {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, evaluation_id = %eval_id, "Failed to query queued builds for schedulability");
            return true; // fail-safe: don't flip to Waiting on transient error
        }
    };

    if queued.is_empty() {
        return false;
    }

    let active_servers = match EServer::find()
        .filter(
            Condition::all()
                .add(CServer::Organization.eq(org_id))
                .add(CServer::Active.eq(true)),
        )
        .all(&state.db)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to query active servers for schedulability");
            return true;
        }
    };

    if active_servers.is_empty() {
        return false;
    }

    for b in queued {
        let derivation = match EDerivation::find_by_id(b.derivation).one(&state.db).await {
            Ok(Some(d)) => d,
            _ => continue,
        };

        // BUILTIN builds can run anywhere with an active server.
        if derivation.architecture == Architecture::BUILTIN {
            return true;
        }

        // Servers whose architecture matches the derivation.
        let mut arch_ids = HashSet::new();
        let mut id_cond = Condition::any();
        for s in &active_servers {
            id_cond = id_cond.add(CServerArchitecture::Server.eq(s.id));
        }
        match EServerArchitecture::find()
            .filter(
                Condition::all()
                    .add(id_cond)
                    .add(CServerArchitecture::Architecture.eq(derivation.architecture.clone())),
            )
            .all(&state.db)
            .await
        {
            Ok(rows) => {
                for r in rows {
                    arch_ids.insert(r.server);
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to query server architectures for schedulability");
                return true;
            }
        }

        if arch_ids.is_empty() {
            continue;
        }

        let features: Vec<Uuid> = match EDerivationFeature::find()
            .filter(CDerivationFeature::Derivation.eq(derivation.id))
            .all(&state.db)
            .await
        {
            Ok(f) => f.into_iter().map(|f| f.feature).collect(),
            Err(e) => {
                warn!(error = %e, "Failed to query derivation features for schedulability");
                return true;
            }
        };

        if features.is_empty() {
            return true;
        }

        let mut srv_cond = Condition::any();
        for id in &arch_ids {
            srv_cond = srv_cond.add(CServerFeature::Server.eq(*id));
        }
        let mut feat_cond = Condition::any();
        for f in &features {
            feat_cond = feat_cond.add(CServerFeature::Feature.eq(*f));
        }
        let rows = match EServerFeature::find()
            .filter(Condition::all().add(srv_cond).add(feat_cond))
            .all(&state.db)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Failed to query server features for schedulability");
                return true;
            }
        };
        let mut count_per_server: HashMap<Uuid, usize> = HashMap::new();
        for row in rows {
            *count_per_server.entry(row.server).or_insert(0) += 1;
        }
        if count_per_server.values().any(|c| *c >= features.len()) {
            return true;
        }
    }

    false
}

/// Reconciles `Building ↔ Waiting` evaluation states each scheduler tick.
///
/// - `Building` evaluation with queued builds but no server matches their
///   architecture/features → `Waiting`
/// - `Waiting` evaluation whose org now has at least one matching server → `Building`
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

        let schedulable = any_queued_build_schedulable(&state, eval.id, org_id).await;

        if eval.status == EvaluationStatus::Building && !schedulable {
            // Only flip to Waiting if there are still queued builds — an eval
            // with zero queued builds is either done or about to be resolved
            // by `check_evaluation_status`.
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
                info!(evaluation_id = %eval.id, "No schedulable server for queued builds, moving evaluation to Waiting");
                update_evaluation_status(Arc::clone(&state), eval, EvaluationStatus::Waiting).await;
            }
        } else if eval.status == EvaluationStatus::Waiting && schedulable {
            info!(evaluation_id = %eval.id, "Matching server now available, resuming evaluation to Building");
            update_evaluation_status(Arc::clone(&state), eval, EvaluationStatus::Building).await;
        }
    }
}

/// Returns the direct dependencies of a build as `(build, derivation)` pairs
/// belonging to the same evaluation. Walks `derivation_dependency` from the
/// build's derivation and joins back to builds of the current evaluation.
async fn get_build_dependencies(
    state: Arc<ServerState>,
    build: &MBuild,
) -> Result<Vec<(MBuild, MDerivation)>, String> {
    let edges = match EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(build.derivation))
        .all(&state.db)
        .await
    {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to query derivation dependencies");
            return Err("Failed to query derivation dependencies".to_string());
        }
    };

    if edges.is_empty() {
        return Ok(vec![]);
    }

    let mut out = Vec::with_capacity(edges.len());
    for edge in edges {
        let dep_build = match EBuild::find()
            .filter(CBuild::Evaluation.eq(build.evaluation))
            .filter(CBuild::Derivation.eq(edge.dependency))
            .one(&state.db)
            .await
        {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                error!(error = %e, "Failed to query dependency build");
                return Err("Failed to query dependency build".to_string());
            }
        };
        let dep_derivation = match EDerivation::find_by_id(edge.dependency)
            .one(&state.db)
            .await
        {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                error!(error = %e, "Failed to query dependency derivation");
                return Err("Failed to query dependency derivation".to_string());
            }
        };
        out.push((dep_build, dep_derivation));
    }

    Ok(out)
}

/// Resolves a build's dependencies to a flat list of already-built store paths,
/// filtering out any that are still missing from the local store.
async fn get_build_dependencies_sorted(
    state: Arc<ServerState>,
    local_store: &mut LocalDaemonClient,
    build: &MBuild,
) -> Result<Vec<String>, String> {
    let direct = match get_build_dependencies(Arc::clone(&state), build).await {
        Ok(deps) => deps,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to get build dependencies for sorting");
            return Err(e);
        }
    };

    let mut dependencies = HashSet::new();
    for (_dep_build, dep_derivation) in &direct {
        let dep_full_path = nix_store_path(&dep_derivation.derivation_path);
        let output_map = if dep_derivation.derivation_path.ends_with(".drv") {
            let mut deps = get_output_paths(dep_full_path.clone(), local_store)
                .await
                .map_err(|e| {
                    error!(error = %e, derivation_path = %dep_derivation.derivation_path, "Failed to get output path for dependency");
                    "Failed to get output path for dependency".to_string()
                })?
                .values()
                .cloned()
                .collect::<Vec<String>>();

            let missing = state
                .nix_store
                .query_missing_paths(deps.clone())
                .await
                .map_err(|e| {
                    error!(error = %e, derivation_path = %dep_derivation.derivation_path, "Failed to get missing builds for dependency");
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
