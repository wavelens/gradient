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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use entity::evaluation::EvaluationStatus;
use gradient_core::ci::TriggerError;
use gradient_core::sources::{check_project_updates, get_commit_info};
use gradient_core::types::input::vec_to_hex;
use gradient_core::types::wildcard::Wildcard;
use gradient_core::types::*;
use sea_orm::{ActiveModelTrait as _, ColumnTrait, EntityTrait, QueryFilter};

/// Fallback poll interval for projects whose org has a forge webhook configured.
/// Webhooks are the primary trigger; this catches any that fail to arrive.
const WEBHOOK_BACKUP_POLL_SECS: i64 = 1800; // 30 minutes
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::Scheduler;
use super::jobs::{PendingBuildJob, PendingEvalJob};
use gradient_core::types::proto::{BuildJob, BuildTask, FlakeJob, FlakeTask};

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
/// Projects whose organization has a forge webhook configured are polled every
/// [`WEBHOOK_BACKUP_POLL_SECS`] (30 min) as a fallback in case a webhook delivery
/// fails. Projects without a webhook use the CLI `evaluation_timeout`.
pub(crate) async fn poll_projects_for_evaluations(scheduler: &Scheduler) -> anyhow::Result<()> {
    let state = &scheduler.state;
    let now = Utc::now().naive_utc();
    let threshold = now - chrono::Duration::seconds(state.cli.evaluation_timeout);
    let webhook_threshold = now - chrono::Duration::seconds(WEBHOOK_BACKUP_POLL_SECS);

    // Single query joining organization to apply different thresholds per project.
    // LEFT JOIN evaluation so new projects (no last_evaluation) are also included.
    // Terminal statuses: 5=Completed, 6=Failed, 7=Aborted.
    let sql = sea_orm::Statement::from_string(
        sea_orm::DbBackend::Postgres,
        format!(
            r#"
            SELECT p.*
            FROM project p
            JOIN organization o ON p.organization = o.id
            LEFT JOIN evaluation e ON p.last_evaluation = e.id
            WHERE p.active = true
            AND (
                (o.forge_webhook_secret IS NULL     AND p.last_check_at <= '{threshold}')
                OR
                (o.forge_webhook_secret IS NOT NULL AND p.last_check_at <= '{webhook_threshold}')
            )
            AND (
                e.status IN (5, 6, 7)
                OR p.force_evaluation = true
                OR p.last_evaluation IS NULL
            )
            ORDER BY p.last_check_at ASC
            "#,
            threshold = threshold.format("%Y-%m-%d %H:%M:%S%.f"),
            webhook_threshold = webhook_threshold.format("%Y-%m-%d %H:%M:%S%.f"),
        ),
    );

    let projects = EProject::find().from_raw_sql(sql).all(&state.db).await?;

    for project in &projects {
        let (has_update, commit_hash) = match check_project_updates(Arc::clone(state), project)
            .await
        {
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
                // Clear force_evaluation so the project is not re-evaluated on every
                // subsequent poll cycle.
                let mut ap: AProject = project.clone().into();
                ap.force_evaluation = sea_orm::ActiveValue::Set(false);
                ap.last_check_at = sea_orm::ActiveValue::Set(Utc::now().naive_utc());
                if let Err(e) = ap.update(&state.db).await {
                    warn!(project = %project.name, error = %e, "failed to clear force_evaluation");
                }
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

pub(crate) async fn dispatch_queued_evals(scheduler: &Scheduler) -> anyhow::Result<()> {
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
            tasks: vec![
                FlakeTask::FetchFlake,
                FlakeTask::EvaluateFlake,
                FlakeTask::EvaluateDerivations,
            ],
            repository: eval.repository.clone(),
            commit: commit_sha,
            wildcards: eval.wildcard
                .parse::<Wildcard>()
                .map(|w| w.patterns().to_vec())
                .unwrap_or_else(|_| vec![eval.wildcard.clone()]),
            timeout_secs: None,
            sign: None,
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
        // After dispatching, reconcile each in-flight evaluation's
        // Building/Waiting state so the UI reflects "no worker can pick this
        // up" (or recovers when a worker comes back online). Cheap when
        // there are no in-flight evals.
        if let Err(e) = scheduler.reconcile_waiting_state().await {
            error!(error = %e, "reconcile_waiting_state in dispatch loop failed");
        }
    }
}

/// All DB data needed to assemble [`PendingBuildJob`]s for a dispatch pass.
///
/// Loaded in bulk (one IN-list query per table) by [`BuildDispatchMaps::load`],
/// then queried in-memory by [`BuildDispatchMaps::make_pending_job`].
/// This avoids O(n) serial round-trips when hundreds of builds become ready
/// at once.
struct BuildDispatchMaps {
    derivations: HashMap<Uuid, MDerivation>,
    evaluations: HashMap<Uuid, MEvaluation>,
    /// project_id → organization_id
    projects: HashMap<Uuid, Uuid>,
    /// eval_id → organization_id (used when the eval has no project)
    direct_builds: HashMap<Uuid, Uuid>,
    features_by_drv: HashMap<Uuid, Vec<Uuid>>,
    feature_names: HashMap<Uuid, String>,
}

impl BuildDispatchMaps {
    /// Issue one IN-list query per table and build all lookup maps.
    async fn load(state: &Arc<ServerState>, builds: &[MBuild]) -> anyhow::Result<Self> {
        let drv_ids: Vec<Uuid> = builds.iter().map(|b| b.derivation).collect();
        let eval_ids: Vec<Uuid> = builds
            .iter()
            .map(|b| b.evaluation)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let derivations: HashMap<Uuid, MDerivation> = EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids.clone()))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|d| (d.id, d))
            .collect();

        let evaluations: HashMap<Uuid, MEvaluation> = EEvaluation::find()
            .filter(CEvaluation::Id.is_in(eval_ids))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|e| (e.id, e))
            .collect();

        // peer_id resolution: project (preferred) or direct_build (fallback).
        let project_ids: Vec<Uuid> = evaluations
            .values()
            .filter_map(|e| e.project)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let projects: HashMap<Uuid, Uuid> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            EProject::find()
                .filter(CProject::Id.is_in(project_ids))
                .all(&state.db)
                .await?
                .into_iter()
                .map(|p| (p.id, p.organization))
                .collect()
        };
        let direct_builds: HashMap<Uuid, Uuid> = {
            let evals_without_project: Vec<Uuid> = evaluations
                .values()
                .filter(|e| e.project.is_none())
                .map(|e| e.id)
                .collect();
            if evals_without_project.is_empty() {
                HashMap::new()
            } else {
                EDirectBuild::find()
                    .filter(CDirectBuild::Evaluation.is_in(evals_without_project))
                    .all(&state.db)
                    .await?
                    .into_iter()
                    .map(|db| (db.evaluation, db.organization))
                    .collect()
            }
        };

        // Required features: per-derivation list of feature names.
        let feature_edges = EDerivationFeature::find()
            .filter(CDerivationFeature::Derivation.is_in(drv_ids))
            .all(&state.db)
            .await
            .unwrap_or_default();
        let mut features_by_drv: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for e in &feature_edges {
            features_by_drv
                .entry(e.derivation)
                .or_default()
                .push(e.feature);
        }
        let feature_names: HashMap<Uuid, String> = if feature_edges.is_empty() {
            HashMap::new()
        } else {
            let feature_ids: Vec<Uuid> = feature_edges.iter().map(|e| e.feature).collect();
            EFeature::find()
                .filter(CFeature::Id.is_in(feature_ids))
                .all(&state.db)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|f| (f.id, f.name))
                .collect()
        };

        Ok(Self {
            derivations,
            evaluations,
            projects,
            direct_builds,
            features_by_drv,
            feature_names,
        })
    }

    /// Resolve the organization that owns this evaluation (used as `peer_id`
    /// to route the job only to workers registered by that org).
    fn resolve_peer_id(&self, eval: &MEvaluation) -> Option<Uuid> {
        match eval.project {
            Some(pid) => self.projects.get(&pid).copied(),
            None => self.direct_builds.get(&eval.id).copied(),
        }
    }

    /// Return the required Nix system features for `derivation_id`.
    fn required_features(&self, derivation_id: Uuid) -> Vec<String> {
        self.features_by_drv
            .get(&derivation_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| self.feature_names.get(i).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Assemble a `(job_id, PendingBuildJob)` pair for `build`, or `None` if
    /// any required map lookup fails (logged as errors at call site).
    fn make_pending_job(&self, build: &MBuild) -> Option<(String, PendingBuildJob)> {
        let derivation = self.derivations.get(&build.derivation).or_else(|| {
            error!(build_id = %build.id, "derivation not found for build");
            None
        })?;
        let eval = self.evaluations.get(&build.evaluation).or_else(|| {
            error!(build_id = %build.id, "evaluation not found for build");
            None
        })?;
        let peer_id = self.resolve_peer_id(eval).or_else(|| {
            error!(build_id = %build.id, "could not resolve peer_id for build");
            None
        })?;

        let job_id = format!("build:{}", build.id);
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
            architecture: derivation.architecture.clone(),
            required_features: self.required_features(build.derivation),
        };

        Some((job_id, pending))
    }
}

pub(crate) async fn dispatch_ready_builds(scheduler: &Scheduler) -> anyhow::Result<()> {
    let state = &scheduler.state;

    // Ready builds: status = Queued AND every dependency build is Completed (3) or Substituted (7).
    // Ordered by dependency count desc (integration builds first), then by age.
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

    let started = std::time::Instant::now();
    let builds = EBuild::find()
        .from_raw_sql(builds_sql)
        .all(&state.db)
        .await?;
    if builds.is_empty() {
        return Ok(());
    }

    // Filter out builds already in the in-memory tracker — one lock acquisition
    // for the whole pass instead of per-build.
    let new_builds: Vec<MBuild> = {
        let tracker = scheduler.job_tracker.read().await;
        builds
            .into_iter()
            .filter(|b| !tracker.contains_job(&format!("build:{}", b.id)))
            .collect()
    };
    if new_builds.is_empty() {
        return Ok(());
    }

    let maps = BuildDispatchMaps::load(state, &new_builds).await?;

    let mut enqueued = 0usize;
    for build in new_builds {
        let Some((job_id, pending)) = maps.make_pending_job(&build) else {
            continue;
        };
        scheduler.enqueue_build_job(job_id, pending).await;
        enqueued += 1;
    }
    info!(
        enqueued,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "dispatch_ready_builds completed"
    );

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
