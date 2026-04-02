/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use core::executer::get_local_store;
use core::sources::*;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use futures::stream::{self, StreamExt};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::evaluator::evaluate;
use super::status::{
    update_build_status, update_evaluation_status, update_evaluation_status_with_error,
};

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

    let mut local_daemon = match get_local_store(Some(organization)).await {
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

    let builds = evaluate(Arc::clone(&state), &mut local_daemon, &evaluation).await;

    match builds {
        Ok(builds) => {
            // Re-fetch to check if aborted while evaluate() was running
            match EEvaluation::find_by_id(evaluation.id).one(&state.db).await {
                Ok(Some(current)) if current.status == EvaluationStatus::Aborted => return,
                _ => {}
            }

            let (builds, dependencies, entry_point_build_ids) = builds;
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

            if !active_builds.is_empty() {
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

            if let Some(project_id) = evaluation.project {
                let now = chrono::Utc::now().naive_utc();
                let active_entry_points = entry_point_build_ids
                    .iter()
                    .map(|&build_id| AEntryPoint {
                        id: sea_orm::ActiveValue::Set(Uuid::new_v4()),
                        project: sea_orm::ActiveValue::Set(project_id),
                        evaluation: sea_orm::ActiveValue::Set(evaluation.id),
                        build: sea_orm::ActiveValue::Set(build_id),
                        created_at: sea_orm::ActiveValue::Set(now),
                    })
                    .collect::<Vec<AEntryPoint>>();

                if !active_entry_points.is_empty() {
                    const BATCH_SIZE: usize = 1000;
                    for chunk in active_entry_points.chunks(BATCH_SIZE) {
                        if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                            .exec(&state.db)
                            .await
                        {
                            error!(error = %e, "Failed to insert entry points");
                        }
                    }
                }
            }

            // Transition all Created builds for this evaluation to Queued now that
            // their dependency records are fully inserted. This covers both newly
            // created builds and clones of previously-failed builds.
            let created_builds = EBuild::find()
                .filter(CBuild::Evaluation.eq(evaluation.id))
                .filter(CBuild::Status.eq(BuildStatus::Created))
                .all(&state.db)
                .await
                .unwrap_or_default();

            if created_builds.is_empty() {
                update_evaluation_status(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Completed,
                )
                .await;
                return;
            }

            for build in created_builds {
                update_build_status(Arc::clone(&state), build, BuildStatus::Queued).await;
            }

            info!("Starting evaluation build phase");
            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Building)
                .await;
        }

        Err(e) => {
            error!(error = %format!("{:#}", e), "Failed to evaluate");
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

/// Polls for the next project that is due for evaluation, creates the evaluation and commit
/// records, and returns the evaluation to run. Loops until one is found.
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
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Aborted))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Queued)),
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
                                updated_at: Utc::now().naive_utc(),
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

        // If the evaluation was pre-created as Queued (e.g. via the API endpoint), use it
        // directly without creating a new one.
        if evaluation.status == EvaluationStatus::Queued && evaluation.id != Uuid::nil() {
            let mut active_project: AProject = project.clone().into();
            active_project.last_check_at = Set(Utc::now().naive_utc());
            active_project.force_evaluation = Set(false);
            if let Err(e) = active_project.update(&state.db).await {
                error!(error = %e, "Failed to update project");
            }
            return evaluation.clone();
        }

        let evaluation_id = if evaluation.id == Uuid::nil() {
            None
        } else {
            Some(evaluation.id)
        };

        // Guard against the race condition where the API endpoint (or a
        // concurrent fetcher tick) already created a Queued/Evaluating/Building
        // evaluation while we were checking for updates with stale project data.
        match EEvaluation::find()
            .filter(CEvaluation::Project.eq(project.id))
            .filter(
                Condition::any()
                    .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                    .add(CEvaluation::Status.eq(EvaluationStatus::Evaluating))
                    .add(CEvaluation::Status.eq(EvaluationStatus::Building)),
            )
            .one(&state.db)
            .await
        {
            Ok(Some(existing)) => {
                // An evaluation is already in progress — reuse it and bail out.
                let mut active_project: AProject = project.clone().into();
                active_project.last_check_at = Set(Utc::now().naive_utc());
                active_project.force_evaluation = Set(false);
                if let Err(e) = active_project.update(&state.db).await {
                    error!(error = %e, "Failed to update project after in-progress guard");
                }
                return existing;
            }
            Ok(None) => {}
            Err(e) => {
                error!(error = %e, "Failed to check for in-progress evaluations");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }

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
        let now = Utc::now().naive_utc();
        let new_evaluation = AEvaluation {
            id: Set(Uuid::new_v4()),
            project: Set(Some(project.id)),
            repository: Set(project.repository.clone()),
            commit: Set(commit.id),
            wildcard: Set(project.evaluation_wildcard.clone()),
            status: Set(EvaluationStatus::Queued),
            previous: Set(evaluation_id),
            next: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
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
