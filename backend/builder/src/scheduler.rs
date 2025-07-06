/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use core::executer::*;
use core::sources::*;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use futures::stream::{self, StreamExt};
use nix_daemon::nix::DaemonStore;
use nix_daemon::{Progress, Store};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::evaluator::*;

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

    let (project, organization) = if let Some(project_id) = evaluation.project {
        // Regular project-based evaluation
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        let organization = EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();
        (Some(project), organization)
    } else {
        // Direct build - get organization from DirectBuild record
        let direct_build = EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        let organization = EOrganization::find_by_id(direct_build.organization)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();
        (None, organization)
    };

    let local_daemon = match get_local_store(Some(organization)).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to get local store");
            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Failed)
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

            if state.cli.debug {
                for build in &builds {
                    debug!(build_id = %build.id, derivation_path = %build.derivation_path, "Created build");
                }
                for dep in &dependencies {
                    debug!(build = %dep.build, dependency = %dep.dependency, "Created dependency");
                }
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

            EBuild::insert_many(active_builds)
                .exec(&state.db)
                .await
                .unwrap();

            if !active_dependencies.is_empty() {
                EBuildDependency::insert_many(active_dependencies)
                    .exec(&state.db)
                    .await
                    .unwrap();

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
            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Failed)
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

    let organization = EOrganization::find_by_id(server.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let mut local_daemon = match get_local_store(Some(organization.clone())).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to get local store for build");
            update_build_status_recursivly(state, build.clone(), BuildStatus::Aborted).await;
            return;
        }
    };

    let (private_key, public_key) =
        decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization).unwrap();

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

    // Get all dependencies in topological order from the database
    let dependencies = get_build_dependencies_sorted(Arc::clone(&state), &build)
        .await
        .unwrap();

    info!(
        dependency_count = dependencies.len(),
        "Copying dependencies in order"
    );
    if state.cli.debug {
        for (i, dep) in dependencies.iter().enumerate() {
            debug!(index = i, dependency = %dep, "Dependency order");
        }
    }

    match local_daemon {
        LocalNixStore::UnixStream(ref mut store) => {
            copy_builds(dependencies.clone(), store, &mut server_daemon, false).await
        }
        LocalNixStore::CommandDuplex(ref mut store) => {
            copy_builds(dependencies.clone(), store, &mut server_daemon, false).await
        }
    }
    .unwrap();

    let mut build_outputs: Vec<ABuildOutput> = vec![];

    match execute_build(&build, &mut server_daemon, Arc::clone(&state)).await {
        Ok(results) => {
            let status = if results.1.values().all(|r| r.error_msg.is_empty()) {
                for build_result in results.1.values() {
                    let build_results = build_result
                        .built_outputs
                        .clone()
                        .into_iter()
                        .map(|(_output, path)| {
                            format!(
                                "/nix/store/{}",
                                serde_json::from_str::<BuildOutputPath>(&path)
                                    .unwrap()
                                    .out_path
                            )
                        })
                        .collect::<Vec<_>>();
                    match local_daemon {
                        LocalNixStore::UnixStream(ref mut store) => {
                            copy_builds(build_results, &mut server_daemon, store, true).await
                        }
                        LocalNixStore::CommandDuplex(ref mut store) => {
                            copy_builds(build_results, &mut server_daemon, store, true).await
                        }
                    }
                    .unwrap();

                    for (build_output, build_output_path) in build_result.built_outputs.clone() {
                        let build_output_path =
                            serde_json::from_str::<BuildOutputPath>(&build_output_path).unwrap();

                        let (build_output_path_hash, build_output_path_package) =
                            get_hash_from_path(format!(
                                "/nix/store/{}",
                                build_output_path.out_path
                            ))
                            .unwrap();

                        build_outputs.push(ABuildOutput {
                            id: Set(Uuid::new_v4()),
                            build: Set(build.id),
                            output: Set(build_output),
                            hash: Set(build_output_path_hash),
                            package: Set(build_output_path_package),
                            file_hash: Set(None),
                            file_size: Set(None),
                            is_cached: Set(false),
                            ca: Set(None),
                            created_at: Set(Utc::now().naive_utc()),
                        });
                    }
                }

                BuildStatus::Completed
            } else {
                for (path, result) in results.1 {
                    if !result.error_msg.is_empty() {
                        error!(path = %path, error = %result.error_msg, "Build failed");
                    }
                }

                BuildStatus::Failed
            };

            build = results.0;

            update_build_status(Arc::clone(&state), build.clone(), status).await;
            check_evaluation_status(Arc::clone(&state), build.evaluation).await;
        }

        Err(e) => {
            error!(error = %e, "Failed to execute build");
            update_build_status_recursivly(Arc::clone(&state), build.clone(), BuildStatus::Failed)
                .await;
        }
    };

    if !build_outputs.is_empty() {
        EBuildOutput::insert_many(build_outputs)
            .exec(&state.db)
            .await
            .unwrap();
    }
}

async fn get_next_evaluation(state: Arc<ServerState>) -> MEvaluation {
    loop {
        let threshold_time =
            Utc::now().naive_utc() - chrono::Duration::seconds(state.cli.evaluation_timeout);

        let mut projects = EProject::find()
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
            .unwrap();

        projects.extend(
            EProject::find()
                .filter(
                    Condition::all()
                        .add(CProject::Active.eq(true))
                        .add(CProject::LastCheckAt.lte(threshold_time))
                        .add(CProject::LastEvaluation.is_null()),
                )
                .order_by_asc(CProject::LastCheckAt)
                .all(&state.db)
                .await
                .unwrap(),
        );

        let mut i_offset = 0;
        for (i, project) in projects.clone().iter().enumerate() {
            let has_no_servers = EServer::find()
                .filter(
                    Condition::all()
                        .add(CServer::Active.eq(true))
                        .add(CServer::Organization.eq(project.organization)),
                )
                .one(&state.db)
                .await
                .unwrap()
                .is_none();

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
                        check_project_updates(Arc::clone(&intern_state), &p).await;

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

        let (evaluation, commit_hash) = evaluations.first().unwrap();

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
            match get_commit_info(Arc::clone(&state), &project, &commit_hash).await {
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

        let commit = acommit.insert(&state.db).await.unwrap();

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
        };

        let new_evaluation = new_evaluation.insert(&state.db).await.unwrap();
        info!(evaluation_id = %new_evaluation.id, "Created new evaluation");

        let mut active_project: AProject = project.clone().into();

        active_project.last_check_at = Set(Utc::now().naive_utc());
        active_project.last_evaluation = Set(Some(new_evaluation.id));
        active_project.force_evaluation = Set(false);

        active_project.update(&state.db).await.unwrap();

        if evaluation_id.is_some() {
            let mut active_evaluation: AEvaluation = evaluation.clone().into();
            active_evaluation.next = Set(Some(new_evaluation.id));

            active_evaluation.update(&state.db).await.unwrap();
        };

        return new_evaluation;
    }
}

async fn requeue_build(state: Arc<ServerState>, build: MBuild) -> MBuild {
    let mut build: ABuild = EBuild::find_by_id(build.id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap()
        .into();

    build.status = Set(BuildStatus::Queued);
    build.server = Set(None);
    build.updated_at = Set(Utc::now().naive_utc());
    let build = build.update(&state.db).await.unwrap();

    info!(build_id = %build.id, "Requeueing build");
    build
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

        let builds = EBuild::find()
            .from_raw_sql(builds_sql)
            .all(&state.db)
            .await
            .unwrap();

        debug!(build_count = builds.len(), "Found queued builds");

        for build in builds {
            let evaluation = EEvaluation::find_by_id(build.evaluation)
                .one(&state.db)
                .await
                .unwrap()
                .unwrap();

            let project = if let Some(project_id) = evaluation.project {
                Some(
                    EProject::find_by_id(project_id)
                        .one(&state.db)
                        .await
                        .unwrap()
                        .unwrap(),
                )
            } else {
                None
            };

            let organization_id = if let Some(project) = &project {
                project.organization
            } else {
                // Direct build - get organization from DirectBuild record
                EDirectBuild::find()
                    .filter(CDirectBuild::Evaluation.eq(evaluation.id))
                    .one(&state.db)
                    .await
                    .unwrap()
                    .unwrap()
                    .organization
            };

            let has_servers = EServer::find()
                .filter(
                    Condition::all()
                        .add(CServer::Active.eq(true))
                        .add(CServer::Organization.eq(organization_id)),
                )
                .one(&state.db)
                .await
                .unwrap()
                .is_some();

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

            // Debug: Check what dependencies exist for this build
            if state.cli.debug {
                // First, check raw dependency records
                let raw_deps = EBuildDependency::find()
                    .filter(CBuildDependency::Build.eq(build.id))
                    .all(&state.db)
                    .await
                    .unwrap();

                debug!(
                    build_id = %build.id,
                    derivation_path = %build.derivation_path,
                    raw_dependency_count = raw_deps.len(),
                    "Raw dependency records"
                );

                for dep in &raw_deps {
                    debug!(build = %dep.build, dependency = %dep.dependency, "Raw dependency");
                }

                let dependencies = get_build_dependencies(Arc::clone(&state), &build)
                    .await
                    .unwrap();

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
    let features = EBuildFeature::find()
        .filter(CBuildFeature::Build.eq(build.id))
        .all(&state.db)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.feature)
        .collect::<Vec<Uuid>>();

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let organization_id = if let Some(project_id) = evaluation.project {
        // Regular project-based evaluation
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();
        project.organization
    } else {
        // Direct build - get organization from DirectBuild record
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .organization
    };

    let mut cond = Condition::all()
        .add(CServer::Organization.eq(organization_id))
        .add(CServer::Active.eq(true))
        .add(CServerArchitecture::Architecture.eq(build.architecture.clone()));

    for feature in features {
        cond = cond.add(CServerFeature::Feature.eq(feature));
    }

    let servers = EServer::find()
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
        .unwrap();

    if servers.is_empty() {
        update_build_status_recursivly(state, build.clone(), BuildStatus::Aborted).await;
        warn!(build_id = %build.id, "Aborted build - no servers found");

        return None;
    }

    for s in servers {
        let running_builds = EBuild::find()
            .filter(CBuild::Server.eq(s.id))
            .filter(CBuild::Status.eq(BuildStatus::Building))
            .all(&state.db)
            .await
            .unwrap();

        let mut abuild: ABuild = build.clone().into();
        if running_builds.is_empty() {
            abuild.server = Set(Some(s.id));
            abuild.status = Set(BuildStatus::Building);
            abuild.updated_at = Set(Utc::now().naive_utc());
            let build = abuild.update(&state.db).await.unwrap();

            debug!(build_id = %build.id, "Selected next build");

            return Some((build, s));
        } else {
            abuild.updated_at = Set(Utc::now().naive_utc());
            abuild.update(&state.db).await.unwrap();
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

    let mut active_build: ABuild = build.into_active_model();

    active_build.status = Set(status);
    active_build.updated_at = Set(Utc::now().naive_utc());

    let build = active_build.update(&state.db).await.unwrap();

    build
}

async fn update_build_status_recursivly(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    // TODO: more efficient, recursive till all dependencies are updated
    let dependencies = EBuildDependency::find()
        .filter(CBuildDependency::Dependency.eq(build.id))
        .all(&state.db)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.build)
        .collect::<Vec<Uuid>>();

    let mut condition = Condition::any();

    for dependency in dependencies {
        condition = condition.add(CBuild::Id.eq(dependency));
    }

    let status_condition = if status == BuildStatus::Aborted {
        Condition::any()
            .add(CBuild::Status.eq(BuildStatus::Created))
            .add(CBuild::Status.eq(BuildStatus::Queued))
            .add(CBuild::Status.eq(BuildStatus::Building))
    } else {
        Condition::all().add(CBuild::Status.ne(status.clone()))
    };

    let dependent_builds = EBuild::find()
        .filter(condition)
        .filter(status_condition)
        .all(&state.db)
        .await
        .unwrap();

    for dependent_build in dependent_builds {
        update_build_status(Arc::clone(&state), dependent_build, status.clone()).await;
    }

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

    let mut active_evaluation: AEvaluation = evaluation.into_active_model();
    active_evaluation.status = Set(status);

    active_evaluation.update(&state.db).await.unwrap()
}

pub async fn abort_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    if evaluation.status == EvaluationStatus::Completed {
        return;
    }

    let builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .filter(
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building)),
        )
        .all(&state.db)
        .await
        .unwrap();

    for build in builds {
        update_build_status(Arc::clone(&state), build, BuildStatus::Aborted).await;
    }

    update_evaluation_status(state, evaluation, EvaluationStatus::Aborted).await;
}

async fn check_evaluation_status(state: Arc<ServerState>, evaluation_id: Uuid) {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .all(&state.db)
        .await
        .unwrap();

    let statuses = builds
        .into_iter()
        .map(|b| b.status)
        .collect::<Vec<BuildStatus>>();

    let status = if statuses.iter().all(|s| *s == BuildStatus::Completed) {
        EvaluationStatus::Completed
    } else if statuses.iter().any(|s| *s == BuildStatus::Failed) {
        EvaluationStatus::Failed
    } else if statuses.iter().any(|s| *s == BuildStatus::Aborted) {
        EvaluationStatus::Aborted
    } else {
        return;
    };

    update_evaluation_status(state, evaluation, status).await;
}

async fn get_build_dependencies(
    state: Arc<ServerState>,
    build: &MBuild,
) -> Result<Vec<MBuild>, String> {
    let dependencies = EBuildDependency::find()
        .filter(CBuildDependency::Build.eq(build.id))
        .all(&state.db)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.dependency)
        .collect::<Vec<Uuid>>();

    let mut condition = Condition::any();

    for dependency in dependencies {
        condition = condition.add(CBuild::Id.eq(dependency));
    }

    let builds = EBuild::find()
        .filter(condition)
        .all(&state.db)
        .await
        .unwrap();

    Ok(builds)
}

async fn get_build_dependencies_sorted(
    state: Arc<ServerState>,
    build: &MBuild,
) -> Result<Vec<String>, String> {
    // Get direct dependencies and add them first, then the main build
    let bdependencies = get_build_dependencies(Arc::clone(&state), &build)
        .await
        .unwrap();

    let mut dependencies = Vec::new();

    // Add all dependency derivation paths first (these need to be copied before the main build)
    for dependency in &bdependencies {
        dependencies.push(dependency.derivation_path.clone());
    }

    // Add the main build's derivation path last
    dependencies.push(build.derivation_path.clone());

    Ok(dependencies)
}
