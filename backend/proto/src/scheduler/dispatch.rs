/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background loops that poll the DB and enqueue jobs into the in-memory scheduler.
//!
//! Two loops run concurrently:
//! - `eval_dispatch_loop`: finds `Queued` evaluations → enqueues `FlakeJob`s
//! - `build_dispatch_loop`: finds ready `Queued` builds → enqueues `BuildJob`s
//!
//! Both loops are idempotent: re-enqueueing the same job_id overwrites the
//! existing entry in the `JobTracker` without harm.

use std::sync::Arc;
use std::time::Duration;

use entity::evaluation::EvaluationStatus;
use gradient_core::types::input::vec_to_hex;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::messages::{BuildJob, BuildTask, FlakeJob, FlakeTask};
use super::jobs::{PendingBuildJob, PendingEvalJob};
use super::Scheduler;

/// Spawns both dispatch loops as detached tokio tasks.
pub fn start_dispatch_loops(scheduler: Arc<Scheduler>) {
    let s1 = Arc::clone(&scheduler);
    let s2 = Arc::clone(&scheduler);
    tokio::spawn(async move { eval_dispatch_loop(s1).await });
    tokio::spawn(async move { build_dispatch_loop(s2).await });
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
