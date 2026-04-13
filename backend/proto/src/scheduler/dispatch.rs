/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background loops that poll the DB and enqueue jobs into the in-memory scheduler.
//!
//! Three loops run concurrently:
//! - `project_poll_loop`: polls active projects for new git commits → creates `Queued` evaluations
//! - `eval_dispatch_loop`: finds `Queued` evaluations → enqueues `FlakeJob`s
//! - `build_dispatch_loop`: finds ready `Queued` builds → enqueues `BuildJob`s
//!
//! The eval/build loops are idempotent: re-enqueueing the same job_id overwrites
//! the existing entry in the `JobTracker` without harm.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use entity::evaluation::EvaluationStatus;
use gradient_core::ci::TriggerError;
use gradient_core::sources::{check_project_updates, get_commit_info};
use gradient_core::types::input::vec_to_hex;
use gradient_core::types::*;
use sea_orm::{ActiveModelTrait as _, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QueryOrder, QuerySelect, RelationTrait};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::messages::{BuildJob, BuildTask, FlakeJob, FlakeTask};
use super::jobs::{PendingBuildJob, PendingEvalJob};
use super::Scheduler;

/// Spawns all dispatch loops as detached tokio tasks.
pub fn start_dispatch_loops(scheduler: Arc<Scheduler>) {
    let s1 = Arc::clone(&scheduler);
    let s2 = Arc::clone(&scheduler);
    let s3 = Arc::clone(&scheduler);
    tokio::spawn(async move { project_poll_loop(s3).await });
    tokio::spawn(async move { eval_dispatch_loop(s1).await });
    tokio::spawn(async move { build_dispatch_loop(s2).await });
}

// ── Project polling ──────────────────────────────────────────────────────────

async fn project_poll_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    info!("project poll loop started");
    loop {
        interval.tick().await;
        if let Err(e) = poll_projects_for_evaluations(&scheduler).await {
            error!(error = %e, "project poll error");
        }
    }
}

/// Polls active projects for new git commits and creates `Queued` evaluations.
///
/// For each project that is due for checking (based on `evaluation_timeout`),
/// calls `check_project_updates` to compare the remote HEAD with the last
/// evaluated commit. If an update is found, creates a new `Queued` evaluation
/// via `trigger_evaluation`.
pub(crate) async fn poll_projects_for_evaluations(scheduler: &Arc<Scheduler>) -> anyhow::Result<()> {
    let state = &scheduler.state;
    let threshold_time =
        Utc::now().naive_utc() - chrono::Duration::seconds(state.cli.evaluation_timeout);

    // Find projects with a last evaluation in terminal state that are due for checking.
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
        .await?;

    // Also find projects that have never been evaluated.
    let new_projects = EProject::find()
        .filter(
            Condition::all()
                .add(CProject::Active.eq(true))
                .add(CProject::LastCheckAt.lte(threshold_time))
                .add(CProject::LastEvaluation.is_null()),
        )
        .order_by_asc(CProject::LastCheckAt)
        .all(&state.db)
        .await?;

    projects.extend(new_projects);

    for project in &projects {
        let (has_update, commit_hash) = match check_project_updates(Arc::clone(state), project).await {
            Ok(result) => result,
            Err(e) => {
                warn!(project = %project.name, error = %e, "failed to check project for updates");
                continue;
            }
        };

        if !has_update {
            // Update last_check_at so we don't re-check immediately.
            let mut ap: AProject = project.clone().into();
            ap.last_check_at = sea_orm::ActiveValue::Set(Utc::now().naive_utc());
            if let Err(e) = ap.update(&state.db).await {
                warn!(project = %project.name, error = %e, "failed to update last_check_at");
            }
            continue;
        }

        let (commit_message, _, author_name) =
            match get_commit_info(Arc::clone(state), project, &commit_hash).await {
                Ok(info) => info,
                Err(e) => {
                    warn!(project = %project.name, error = %e, "failed to fetch commit info");
                    (String::new(), None, String::new())
                }
            };

        match gradient_core::ci::trigger_evaluation(
            &state.db,
            project,
            commit_hash,
            Some(commit_message),
            Some(author_name),
        )
        .await
        {
            Ok(eval) => {
                info!(project = %project.name, evaluation_id = %eval.id, "created evaluation from project poll");
            }
            Err(TriggerError::AlreadyInProgress) => {
                debug!(project = %project.name, "evaluation already in progress, skipping");
            }
            Err(e) => {
                error!(project = %project.name, error = %e, "failed to create evaluation");
            }
        }
    }

    Ok(())
}

// ── Eval dispatch ─────────────────────────────────────────────────────────────

async fn eval_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    info!("eval dispatch loop started");
    loop {
        interval.tick().await;
        if let Err(e) = dispatch_queued_evals(&scheduler).await {
            error!(error = %e, "eval dispatch error");
        }
    }
}

pub(crate) async fn dispatch_queued_evals(scheduler: &Arc<Scheduler>) -> anyhow::Result<()> {
    let state = &scheduler.state;

    let evals = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Queued))
        .all(&state.db)
        .await?;

    for eval in evals {
        let job_id = format!("eval:{}", eval.id);

        // Skip if already in the scheduler (pending or active).
        if scheduler.job_tracker.read().await.contains_job(&job_id) {
            continue;
        }

        let commit = match ECommit::find_by_id(eval.commit).one(&state.db).await? {
            Some(c) => c,
            None => {
                error!(evaluation_id = %eval.id, "commit not found for evaluation");
                continue;
            }
        };

        let commit_sha = vec_to_hex(&commit.hash);

        let flake_job = FlakeJob {
            tasks: vec![FlakeTask::FetchFlake, FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations],
            repository: eval.repository.clone(),
            commit: commit_sha,
            wildcards: vec![eval.wildcard.clone()],
            timeout_secs: None,
        };

        let organization_id = organization_id_for_eval(state, &eval).await;
        let org_id = match organization_id {
            Some(id) => id,
            None => {
                error!(evaluation_id = %eval.id, "could not determine organization for evaluation");
                continue;
            }
        };

        let pending = PendingEvalJob {
            evaluation_id: eval.id,
            project_id: eval.project,
            peer_id: org_id,
            commit_id: eval.commit,
            repository: eval.repository.clone(),
            job: flake_job,
            required_paths: vec![],
        };

        scheduler.enqueue_eval_job(job_id.clone(), pending).await;
        debug!(evaluation_id = %eval.id, %job_id, "eval job enqueued");
    }

    Ok(())
}

// ── Build dispatch ────────────────────────────────────────────────────────────

async fn build_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    info!("build dispatch loop started");
    loop {
        interval.tick().await;
        if let Err(e) = dispatch_ready_builds(&scheduler).await {
            error!(error = %e, "build dispatch error");
        }
    }
}

pub(crate) async fn dispatch_ready_builds(scheduler: &Arc<Scheduler>) -> anyhow::Result<()> {
    let state = &scheduler.state;

    // Ready builds: status = Queued AND no unsatisfied dependencies.
    let builds_sql = sea_orm::Statement::from_string(
        sea_orm::DbBackend::Postgres,
        r#"
            SELECT b.*
            FROM public.build b
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
            ORDER BY b.updated_at ASC
        "#,
    );

    let builds = EBuild::find().from_raw_sql(builds_sql).all(&state.db).await?;

    for build in builds {
        let job_id = format!("build:{}", build.id);

        if scheduler.job_tracker.read().await.contains_job(&job_id) {
            continue;
        }

        let derivation = match EDerivation::find_by_id(build.derivation).one(&state.db).await? {
            Some(d) => d,
            None => {
                error!(build_id = %build.id, "derivation not found for build");
                continue;
            }
        };

        let eval = match EEvaluation::find_by_id(build.evaluation).one(&state.db).await? {
            Some(e) => e,
            None => {
                error!(build_id = %build.id, "evaluation not found for build");
                continue;
            }
        };

        let peer_id = match organization_id_for_eval(state, &eval).await {
            Some(id) => id,
            None => {
                error!(build_id = %build.id, "could not determine peer for build");
                continue;
            }
        };

        let build_job = BuildJob {
            builds: vec![BuildTask {
                build_id: build.id.to_string(),
                drv_path: derivation.derivation_path.clone(),
            }],
            compress: None,
            sign: None,
        };

        let pending = PendingBuildJob {
            build_id: build.id,
            evaluation_id: build.evaluation,
            peer_id,
            job: build_job,
            required_paths: vec![],
        };

        scheduler.enqueue_build_job(job_id.clone(), pending).await;
        debug!(build_id = %build.id, %job_id, "build job enqueued");
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn organization_id_for_eval(state: &Arc<ServerState>, eval: &MEvaluation) -> Option<Uuid> {
    if let Some(project_id) = eval.project {
        match EProject::find_by_id(project_id).one(&state.db).await {
            Ok(Some(p)) => return Some(p.organization),
            Ok(None) => return None,
            Err(e) => {
                error!(error = %e, %project_id, "failed to load project for eval");
                return None;
            }
        }
    }

    // Direct build: look up DirectBuild record.
    match EDirectBuild::find()
        .filter(CDirectBuild::Evaluation.eq(eval.id))
        .one(&state.db)
        .await
    {
        Ok(Some(db)) => Some(db.organization),
        Ok(None) => None,
        Err(e) => {
            error!(error = %e, evaluation_id = %eval.id, "failed to load direct_build for eval");
            None
        }
    }
}
