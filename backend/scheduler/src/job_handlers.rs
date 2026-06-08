/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `Scheduler` methods for job queuing, assignment, status updates,
//! completion, log streaming, abort, and diagnostics.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use entity::build::BuildStatus;
use sea_orm::EntityTrait;
use sea_orm::{ActiveModelTrait, ColumnTrait, IntoActiveModel, QueryFilter, Set};
use tracing::{debug, error, info, warn};

use gradient_core::executer::strip_nix_store_prefix;
use gradient_core::types::proto::{
    BuildFailureKind, BuildMetrics, BuildOutput, CandidateScore, DiscoveredDerivation, JobCandidate,
    JobKind,
};

use gradient_core::types::*;

use crate::Scheduler;
use crate::jobs::{
    Assignment, DispatchRecord, PendingBuildJob, PendingEvalJob, PendingJob, WorkerCaps,
};
use crate::worker_pool::WorkerInfo;
use crate::{build, dispatch, eval};

impl Scheduler {
    // ── Job queue ─────────────────────────────────────────────────────────────

    pub async fn enqueue_eval_job(&self, job_id: String, job: PendingEvalJob) -> JobCandidate {
        let candidate = self
            .job_tracker
            .write()
            .await
            .add_pending(job_id, PendingJob::Eval(job));
        self.job_notify.notify_waiters();
        candidate
    }

    pub async fn enqueue_build_job(&self, job_id: String, job: PendingBuildJob) -> JobCandidate {
        let candidate = self
            .job_tracker
            .write()
            .await
            .add_pending(job_id, PendingJob::Build(job));
        self.job_notify.notify_waiters();
        candidate
    }

    /// Returns a handle that handler dispatch loops can `await` on to be woken
    /// when new jobs are enqueued in the scheduler.
    pub fn job_notify(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.job_notify)
    }

    /// Returns ALL pending job candidates visible to the given worker.
    /// Used for `RequestJobList` / `RequestAllCandidates` (full snapshot).
    /// Also marks all returned IDs as sent so delta pushes skip them.
    pub async fn get_job_candidates(&self, worker_id: &str) -> Vec<JobCandidate> {
        let (authorized, caps) = self.worker_auth_and_caps(worker_id).await;
        let candidates = self
            .job_tracker
            .read()
            .await
            .candidates_for_worker(authorized.as_ref(), caps.as_ref());
        // Mark all as sent so the next delta push skips them.
        let ids: Vec<String> = candidates.iter().map(|c| c.job_id.clone()).collect();
        self.worker_pool
            .write()
            .await
            .mark_candidates_sent(worker_id, &ids);
        candidates
    }

    /// Returns only NEW pending job candidates (not yet sent to this worker).
    /// Used for incremental `JobOffer` pushes after `job_notify`.
    /// Marks the returned IDs as sent.
    pub async fn get_new_job_candidates(&self, worker_id: &str) -> Vec<JobCandidate> {
        let (authorized, caps) = self.worker_auth_and_caps(worker_id).await;
        let sent = {
            let pool = self.worker_pool.read().await;
            pool.sent_candidates_for(worker_id)
                .cloned()
                .unwrap_or_default()
        };
        let all = self
            .job_tracker
            .read()
            .await
            .candidates_for_worker(authorized.as_ref(), caps.as_ref());
        let new_candidates: Vec<JobCandidate> = all
            .into_iter()
            .filter(|c| !sent.contains(&c.job_id))
            .collect();
        if !new_candidates.is_empty() {
            let ids: Vec<String> = new_candidates.iter().map(|c| c.job_id.clone()).collect();
            self.worker_pool
                .write()
                .await
                .mark_candidates_sent(worker_id, &ids);
        }
        new_candidates
    }

    // ── Scoring / assignment ──────────────────────────────────────────────────

    /// Try to directly assign a job of `kind` to `peer_id` without scoring.
    ///
    /// Called when the worker sends `RequestJob { kind }` to signal it has a
    /// free slot.  Returns `Some(Assignment)` if a matching pending job was
    /// found and claimed; `None` if no such job exists yet.
    pub async fn request_job(&self, peer_id: &str, kind: JobKind) -> Option<Assignment> {
        // ── Server-side capacity guard ──────────────────────────────────────
        {
            let pool = self.worker_pool.read().await;
            if !pool.has_capacity(peer_id, &kind) {
                debug!(%peer_id, ?kind, "RequestJob ignored - worker at capacity");
                return None;
            }
        }

        let (authorized, caps) = self.worker_auth_and_caps(peer_id).await;

        // ── First try: pick from what's already in the tracker ──────────────
        if let Some(a) = self
            .try_assign(peer_id, authorized.as_ref(), caps.as_ref(), &kind)
            .await
        {
            info!(%peer_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob");
            return Some(a);
        }

        // ── Tracker empty: on-demand DB refresh ─────────────────────────────
        // Build dispatch is demand-driven: instead of a 5-second polling
        // loop that blocks the handler, we only query the DB when a worker
        // actually asks for work and the tracker has nothing. This is the
        // ONLY place dispatch_ready_builds runs for build jobs.
        if matches!(kind, JobKind::Build) {
            if let Err(e) = dispatch::dispatch_ready_builds(self).await {
                warn!(error = %e, "on-demand dispatch_ready_builds failed");
            }
            // Also reconcile Waiting/Building state while we're at it.
            if let Err(e) = self.reconcile_waiting_state().await {
                warn!(error = %e, "reconcile_waiting_state after on-demand dispatch failed");
            }
        }

        // ── Second try after refresh ────────────────────────────────────────
        if let Some(a) = self
            .try_assign(peer_id, authorized.as_ref(), caps.as_ref(), &kind)
            .await
        {
            info!(%peer_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob (after DB refresh)");
            return Some(a);
        }

        None
    }

    /// Record candidate scores from a worker. Does NOT assign - the worker
    /// explicitly signals capacity via `RequestJob`. Scores are used later
    /// by `request_job` to pick the best candidate.
    pub async fn record_scores(&self, peer_id: &str, scores: Vec<CandidateScore>) {
        self.job_tracker
            .write()
            .await
            .record_scores(peer_id, scores);
    }

    pub async fn job_rejected(&self, peer_id: &str, job_id: &str) {
        self.worker_pool.write().await.release_job(peer_id, job_id);
        self.job_tracker.write().await.release_to_pending(job_id);
        // Clear the sent-candidate flag so the job shows up in the next delta push.
        self.worker_pool.write().await.remove_sent_candidate(job_id);
        info!(%peer_id, %job_id, "job rejected; re-queued");
    }

    // ── Eval status transitions ───────────────────────────────────────────────

    pub async fn handle_eval_status_update(
        &self,
        job_id: &str,
        new_status: entity::evaluation::EvaluationStatus,
    ) {
        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Eval(j)) => j.evaluation_id,
                _ => return,
            }
        };
        match EEvaluation::find_by_id(evaluation_id)
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(eval)) => {
                gradient_core::db::update_evaluation_status(
                    Arc::clone(&self.state),
                    eval,
                    new_status,
                )
                .await;
            }
            Ok(None) => warn!(%evaluation_id, "evaluation not found for status update"),
            Err(e) => {
                warn!(error = %e, %evaluation_id, "failed to fetch evaluation for status update")
            }
        }
    }

    /// Persist the archived flake store path on the evaluation row so
    /// follow-up eval-only jobs can dispatch with `FlakeSource::Cached`.
    pub async fn persist_flake_source(&self, job_id: &str, flake_source: Option<String>) {
        use sea_orm::ActiveModelTrait;
        use sea_orm::Set;

        let Some(path) = flake_source else { return };
        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Eval(j)) => j.evaluation_id,
                _ => return,
            }
        };
        let am = entity::evaluation::ActiveModel {
            id: Set(evaluation_id),
            flake_source: Set(Some(path)),
            ..Default::default()
        };
        if let Err(e) = am.update(&self.state.worker_db).await {
            warn!(error = %e, %evaluation_id, "failed to persist flake_source");
        }
    }

    pub async fn handle_build_status_update(&self, build_id_str: &str, worker_id: &str) {
        let build_id = match build_id_str.parse::<BuildId>() {
            Ok(id) => id,
            Err(_) => {
                warn!(%build_id_str, "invalid build_id in Building update");
                return;
            }
        };

        match EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(build)) => {
                // Record which worker is handling this build. Persisting here
                // (rather than at dispatch time) means the worker that actually
                // accepted the job is recorded, not whichever candidate the
                // scheduler offered it to.
                if build.worker.as_deref() != Some(worker_id) {
                    let mut am: ABuild = build.clone().into_active_model();
                    am.worker = Set(Some(worker_id.to_string()));
                    if let Err(e) = am.update(&self.state.worker_db).await {
                        warn!(error = %e, %build_id, %worker_id, "failed to persist worker on build");
                    }
                }
                gradient_core::db::update_build_status(
                    Arc::clone(&self.state),
                    build,
                    BuildStatus::Building,
                )
                .await;
            }
            Ok(None) => warn!(%build_id, "build not found for Building status update"),
            Err(e) => warn!(error = %e, %build_id, "failed to fetch build for status update"),
        }
    }

    pub async fn handle_eval_result(
        &self,
        job_id: &str,
        mut derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        let job = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Eval(j)) => j.clone(),
                Some(_) => anyhow::bail!("job {} is not an eval job", job_id),
                None => {
                    warn!(%job_id, "eval result for unknown job - ignoring");
                    return Ok(());
                }
            }
        };

        // Canonicalise every store path to its bare `<hash>-<name>` form
        // before it reaches the DB. `derivation.derivation_path` mirrors the
        // narinfo `References:` convention used by `cached_path`: the
        // `/nix/store/` prefix is added back only at the worker / API
        // boundary. Worker batches may arrive prefixed (eval), unprefixed
        // (mixed legacy), or both, so we strip uniformly here and keep one
        // canonical form for every downstream key (insert dedup, deferred
        // dep edges, build dispatch lookup).
        for d in &mut derivations {
            d.drv_path = strip_nix_store_prefix(&d.drv_path);
            for dep in &mut d.dependencies {
                *dep = strip_nix_store_prefix(dep);
            }
        }

        // Accumulate dep edges to flush later (at eval-job-completed) when
        // every derivation row is guaranteed to be in the DB. The BFS walks
        // roots→leaves, so batch N may contain a derivation whose dep lands
        // in batch N+1 - inserting the edge now would FK-fail or silently
        // skip the dep because the row doesn't exist yet.
        let eval_id = job.evaluation_id;
        let dep_pairs: Vec<(String, Vec<String>)> = derivations
            .iter()
            .filter(|d| !d.dependencies.is_empty())
            .map(|d| (d.drv_path.clone(), d.dependencies.clone()))
            .collect();
        if !dep_pairs.is_empty() {
            self.deferred_deps
                .write()
                .await
                .entry(eval_id)
                .or_default()
                .extend(dep_pairs);
        }

        eval::handle_eval_result(&self.state, &job, derivations, warnings, errors).await
    }

    pub async fn handle_build_output(
        &self,
        job_id: &str,
        build_id_str: &str,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        let build_id: BuildId = build_id_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid build_id: {}", build_id_str))?;

        let job = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Build(j)) => j.clone(),
                Some(_) => anyhow::bail!("job {} is not a build job", job_id),
                None => {
                    warn!(%job_id, "build output for unknown job - ignoring");
                    return Ok(());
                }
            }
        };
        build::handle_build_output(&self.state, &job, build_id, outputs, metrics, substituted).await
    }

    // ── Job completion ────────────────────────────────────────────────────────

    pub async fn handle_job_completed(&self, peer_id: &str, job_id: &str) -> Result<()> {
        self.worker_pool.write().await.release_job(peer_id, job_id);
        let job = self.job_tracker.write().await.remove_active(job_id);
        match job {
            Some(PendingJob::Eval(j)) => {
                // Split mode: a fetch-only job just archived the source. Enqueue
                // the cached eval follow-up instead of finalizing - eval has not run.
                if crate::jobs::is_fetch_only(&j.job) {
                    // Reusing the `eval:{id}` job id is safe: remove_active above
                    // already evicted it from the active map.
                    let store_path = EEvaluation::find_by_id(j.evaluation_id)
                        .one(&self.state.worker_db)
                        .await?
                        .and_then(|e| e.flake_source);
                    return match store_path {
                        Some(path) => {
                            let follow_id = format!("eval:{}", j.evaluation_id);
                            self.enqueue_eval_job(follow_id, j.cached_followup(path)).await;
                            info!(evaluation_id = %j.evaluation_id, "fetch complete; enqueued cached eval follow-up");
                            Ok(())
                        }
                        None => {
                            warn!(evaluation_id = %j.evaluation_id, "fetch-only job reported no flake_source; failing eval");
                            eval::handle_eval_job_failed(
                                &self.state,
                                j.evaluation_id,
                                "fetch completed but no flake source was archived",
                            )
                            .await
                        }
                    };
                }

                // Flush deferred dependency edges BEFORE promoting builds,
                // so the dispatch SQL's dep-gating sees the full graph.
                let deferred = self
                    .deferred_deps
                    .write()
                    .await
                    .remove(&j.evaluation_id)
                    .unwrap_or_default();
                if let Err(e) =
                    eval::flush_deferred_deps(&self.state, j.evaluation_id, j.peer_id, deferred)
                        .await
                {
                    error!(error = %e, evaluation_id = %j.evaluation_id, "flush_deferred_deps failed");
                }
                eval::handle_eval_job_completed(&self.state, j.evaluation_id).await
            }
            Some(PendingJob::Build(j)) => {
                build::handle_build_job_completed(&self.state, j.build_id).await
            }
            None => {
                warn!(%job_id, "job_completed for unknown job");
                Ok(())
            }
        }
    }

    pub async fn handle_job_failed(
        &self,
        peer_id: &str,
        job_id: &str,
        error: &str,
        kind: BuildFailureKind,
    ) -> Result<()> {
        self.worker_pool.write().await.release_job(peer_id, job_id);
        let job = self.job_tracker.write().await.remove_active(job_id);
        match job {
            Some(PendingJob::Eval(j)) => {
                eval::handle_eval_job_failed(&self.state, j.evaluation_id, error).await
            }
            Some(PendingJob::Build(j)) => {
                build::handle_build_job_failed(&self.state, j.build_id, error, kind).await
            }
            None => {
                warn!(%job_id, "job_failed for unknown job");
                Ok(())
            }
        }
    }

    // ── Log streaming ─────────────────────────────────────────────────────────

    pub async fn append_log(&self, job_id: &str, task_index: u32, data: Vec<u8>) -> Result<()> {
        let bytes_len = data.len();
        let text = String::from_utf8_lossy(&data);
        let text = text.as_ref();

        let build_id_str = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Build(j)) => j
                    .job
                    .builds
                    .get(task_index as usize)
                    .map(|t| t.build_id.clone()),
                Some(PendingJob::Eval(_)) => {
                    debug!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: job is an eval, not a build");
                    return Ok(());
                }
                None => {
                    // Common shortly after a build finishes: a few in-flight
                    // log lines from the worker arrive after the job has
                    // already been removed from the active tracker. Not an
                    // error - just lost output, log at debug.
                    debug!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: no active job (likely race with completion)");
                    return Ok(());
                }
            }
        };

        let build_id: BuildId = match build_id_str.and_then(|s| s.parse::<BuildId>().ok()) {
            Some(id) => id,
            None => {
                warn!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: build_task index out of range or build_id unparseable");
                return Ok(());
            }
        };

        let log_id = match EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await?
        {
            Some(b) => b.log_id.unwrap_or(b.id),
            None => build_id,
        };

        debug!(%build_id, %log_id, bytes = bytes_len, "appending build log");
        self.state.log_storage.append(log_id, text).await
    }

    // ── Abort ─────────────────────────────────────────────────────────────────

    /// Abort an evaluation: update DB status and send `AbortJob` to any
    /// worker currently executing a job belonging to this evaluation.
    pub async fn abort_evaluation(&self, evaluation: MEvaluation) {
        let evaluation_id = evaluation.id;

        // Update DB (builds → Aborted, eval → Aborted).
        gradient_core::db::abort_evaluation(Arc::clone(&self.state), evaluation).await;

        // Find active jobs for this evaluation and abort them.
        let tracker = self.job_tracker.read().await;
        let pool = self.worker_pool.read().await;

        let to_abort: Vec<(String, String)> = tracker
            .active_jobs()
            .filter_map(|(job_id, worker_id, job)| {
                if job.evaluation_id() == evaluation_id {
                    Some((worker_id.to_owned(), job_id.to_owned()))
                } else {
                    None
                }
            })
            .collect();

        for (worker_id, job_id) in to_abort {
            if pool.send_abort(&worker_id, job_id.clone(), "evaluation aborted".to_owned()) {
                info!(%worker_id, %job_id, %evaluation_id, "sent AbortJob to worker");
            }
        }

        // Also remove any pending (unassigned) jobs for this evaluation.
        drop(pool);
        drop(tracker);
        self.job_tracker
            .write()
            .await
            .remove_pending_for_evaluation(evaluation_id);
    }

    /// Persist a worker-reported message on the evaluation that owns the
    /// given active `job_id`.
    ///
    /// Used for infrastructure-level signals (NAR prefetch failures, transport
    /// errors, etc.) that should surface on the evaluation page even when the
    /// root cause was seen in a sub-job. Build compile failures and
    /// user-initiated aborts deliberately do not flow through here.
    pub async fn record_eval_message(
        &self,
        job_id: &str,
        level: gradient_core::types::proto::EvalMessageLevel,
        source: String,
        message: String,
    ) -> Result<()> {
        use gradient_core::repo::eval::EvalRepo;

        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(j) => j.evaluation_id(),
                None => {
                    debug!(%job_id, "EvalMessage dropped: no active job");
                    return Ok(());
                }
            }
        };

        let entity_level = match level {
            gradient_core::types::proto::EvalMessageLevel::Error => {
                entity::evaluation_message::MessageLevel::Error
            }
            gradient_core::types::proto::EvalMessageLevel::Warning => {
                entity::evaluation_message::MessageLevel::Warning
            }
            gradient_core::types::proto::EvalMessageLevel::Notice => {
                entity::evaluation_message::MessageLevel::Notice
            }
        };

        EvalRepo::new(self.state.worker_db.inner())
            .insert_message(evaluation_id, entity_level, message, Some(source))
            .await
    }

    /// Return the peer (org) UUID that owns the active job, if found.
    pub async fn peer_id_for_job(&self, job_id: &str) -> Option<OrganizationId> {
        self.job_tracker
            .read()
            .await
            .active_job(job_id)
            .map(|j| j.peer_id())
    }

    /// Returns the connected worker's negotiated `GradientCapabilities`,
    /// or `None` if the worker is not connected.
    pub async fn worker_gradient_caps(
        &self,
        worker_id: &str,
    ) -> Option<gradient_core::types::proto::GradientCapabilities> {
        self.worker_pool.read().await.gradient_caps_for(worker_id)
    }

    // ── Diagnostics ───────────────────────────────────────────────────────────

    pub async fn worker_count(&self) -> usize {
        self.worker_pool.read().await.worker_count()
    }

    pub async fn workers_info(&self) -> Vec<WorkerInfo> {
        self.worker_pool.read().await.all_workers()
    }

    pub async fn pending_job_count(&self) -> usize {
        self.job_tracker.read().await.pending_count()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Fetch the peer auth filter and capabilities for a worker from the pool.
    async fn worker_auth_and_caps(
        &self,
        worker_id: &str,
    ) -> (Option<HashSet<OrganizationId>>, Option<WorkerCaps>) {
        let pool = self.worker_pool.read().await;
        let authorized = pool
            .peer_auth_for(worker_id)
            .and_then(|a| a.as_filter())
            .cloned();
        let caps = match (
            pool.gradient_caps_for(worker_id),
            pool.build_caps_for(worker_id),
        ) {
            (Some(g), Some((architectures, system_features))) => {
                let metrics = pool.metrics_for(worker_id);
                Some(WorkerCaps {
                    fetch: g.fetch,
                    architectures,
                    system_features,
                    capabilities: g,
                    metrics,
                })
            }
            _ => None,
        };
        (authorized, caps)
    }

    /// Atomically take the best matching job from the tracker and record the
    /// assignment on the worker pool. Returns `None` if no suitable job exists.
    async fn try_assign(
        &self,
        peer_id: &str,
        authorized: Option<&HashSet<OrganizationId>>,
        caps: Option<&WorkerCaps>,
        kind: &JobKind,
    ) -> Option<Assignment> {
        let policy = Arc::clone(&self.policy);
        let instance = self.instance.load_full();
        let mut assignment = self
            .job_tracker
            .write()
            .await
            .take_best_of_kind(peer_id, authorized, caps, kind, &*policy, &instance);
        if let Some(a) = assignment.as_mut() {
            self.worker_pool
                .write()
                .await
                .assign_job(peer_id, &a.job_id);
            if let Some(record) = a.dispatch_record.take() {
                let _ = self.state.board_events.send(crate::BoardEvent::JobDispatched {
                    organization: record.organization.into(),
                    worker_id: peer_id.to_owned(),
                    kind: record.kind,
                    score: record.score,
                    build_id: record.build_id.map(Into::into),
                    evaluation_id: record.evaluation_id.into(),
                });
                let state = Arc::clone(&self.state);
                let worker = peer_id.to_owned();
                self.state.shutdown.spawn(async move {
                    persist_dispatched_job(&state, &worker, record).await;
                });
            }
        }
        assignment
    }
}

/// Persist a `dispatched_job` row and stamp `build.dispatched_at`. Best-effort:
/// failures are logged so instrumentation can't break dispatch.
async fn persist_dispatched_job(state: &Arc<ServerState>, worker_id: &str, rec: DispatchRecord) {
    let now = now();
    let row = entity::dispatched_job::ActiveModel {
        id: Set(entity::ids::DispatchedJobId::now_v7()),
        kind: Set(rec.kind),
        build_id: Set(rec.build_id),
        evaluation_id: Set(rec.evaluation_id),
        organization: Set(rec.organization),
        project: Set(rec.project),
        derivation: Set(rec.derivation),
        worker_id: Set(worker_id.to_owned()),
        score: Set(rec.score),
        queued_at: Set(rec.queued_at),
        ready_at: Set(Some(rec.ready_at)),
        dispatched_at: Set(now),
        finished_at: Set(None),
        outcome: Set(None),
        score_breakdown: Set(rec.score_breakdown),
        worker_context: Set(rec.worker_context),
        job_context: Set(rec.job_context),
        instance_context: Set(Some(rec.instance_context.clone())),
        candidates: Set(None),
        created_at: Set(now),
    };
    if let Err(e) = entity::dispatched_job::Entity::insert(row)
        .exec(&state.worker_db)
        .await
    {
        warn!(error = %e, "failed to insert dispatched_job");
    }
    if let Some(build_id) = rec.build_id
        && let Err(e) = entity::build::Entity::update_many()
            .col_expr(
                entity::build::Column::DispatchedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(entity::build::Column::Id.eq(build_id))
            .filter(entity::build::Column::DispatchedAt.is_null())
            .exec(&state.worker_db)
            .await
    {
        warn!(error = %e, %build_id, "failed to stamp build dispatched_at");
    }
}
