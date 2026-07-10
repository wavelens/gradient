/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Scoring and job assignment (`RequestJob`).

use std::collections::HashSet;
use std::sync::Arc;

use sea_orm::EntityTrait;
use sea_orm::{ColumnTrait, IntoActiveModel, QueryFilter};
use tracing::{debug, info, warn};

use gradient_core::ServerState;
use gradient_types::proto::{CandidateScore, JobKind};
use gradient_types::*;

use crate::Scheduler;
use crate::dispatch;
use crate::jobs::{Assignment, DispatchRecord, WorkerCaps};

impl Scheduler {
    // ── Scoring / assignment ──────────────────────────────────────────────────

    /// Try to directly assign a job of `kind` to `worker_id` without scoring.
    ///
    /// Called when the worker sends `RequestJob { kind }` to signal it has a
    /// free slot.  Returns `Some(Assignment)` if a matching pending job was
    /// found and claimed; `None` if no such job exists yet.
    pub async fn request_job(&self, worker_id: &str, kind: JobKind) -> Option<Assignment> {
        // ── Server-side capacity guard ──────────────────────────────────────
        {
            let pool = self.worker_pool.read().await;
            if !pool.has_capacity(worker_id, &kind) {
                debug!(%worker_id, ?kind, "RequestJob ignored - worker at capacity");
                return None;
            }
        }

        let (authorized, caps) = self.worker_auth_and_caps(worker_id).await;

        // ── First try: pick from what's already in the tracker ──────────────
        if let Some(a) = self
            .try_assign(worker_id, authorized.as_ref(), caps.as_ref(), &kind)
            .await
        {
            info!(%worker_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob");
            return Some(a);
        }

        // ── Tracker empty: on-demand DB refresh ─────────────────────────────
        // On-demand dispatch: query the DB immediately when a worker asks for
        // work and the tracker has nothing, instead of waiting for the next
        // build_dispatch_loop tick. Complements - does not replace - that
        // periodic 5s loop, which also runs dispatch_ready_builds.
        if matches!(kind, JobKind::Build) {
            if let Err(e) = dispatch::dispatch_ready_builds(self).await {
                warn!(error = %e, "on-demand dispatch_ready_builds failed");
            }
            // Kick the dispatch loop to reconcile Waiting/Building state off the
            // read loop, rather than blocking this worker's next message on it.
            self.kick_dispatch();
        }

        // ── Second try after refresh ────────────────────────────────────────
        if let Some(a) = self
            .try_assign(worker_id, authorized.as_ref(), caps.as_ref(), &kind)
            .await
        {
            info!(%worker_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob (after DB refresh)");
            return Some(a);
        }

        None
    }

    /// Record candidate scores from a worker. Does NOT assign - the worker
    /// explicitly signals capacity via `RequestJob`. Scores are used later
    /// by `request_job` to pick the best candidate.
    pub async fn record_scores(&self, worker_id: &str, scores: Vec<CandidateScore>) {
        self.job_tracker
            .write()
            .await
            .record_scores(worker_id, scores);
    }

    pub async fn job_rejected(&self, worker_id: &str, job_id: &str) {
        self.worker_pool
            .write()
            .await
            .release_job(worker_id, job_id);
        self.job_tracker.write().await.release_to_pending(job_id);
        // Clear the sent-candidate flag so the job shows up in the next delta push.
        self.worker_pool.write().await.remove_sent_candidate(job_id);
        info!(%worker_id, %job_id, "job rejected; re-queued");
    }

    /// Return the org UUID that owns the active job, if found.
    pub async fn org_for_job(&self, job_id: &str) -> Option<OrganizationId> {
        self.job_tracker
            .read()
            .await
            .active_job(job_id)
            .map(|j| j.org_id())
    }

    /// Fetch the peer auth filter and capabilities for a worker from the pool.
    pub(super) async fn worker_auth_and_caps(
        &self,
        worker_id: &str,
    ) -> (Option<HashSet<OrganizationId>>, Option<WorkerCaps>) {
        let pool = self.worker_pool.read().await;
        let authorized = pool
            .peer_auth_for(worker_id)
            .and_then(|a| a.as_filter())
            .cloned();
        (authorized, pool.worker_caps(worker_id))
    }

    /// Atomically take the best matching job from the tracker and record the
    /// assignment on the worker pool. Returns `None` if no suitable job exists.
    async fn try_assign(
        &self,
        worker_id: &str,
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
            .take_best_of_kind(worker_id, authorized, caps, kind, &*policy, &instance);
        if let Some(a) = assignment.as_mut() {
            self.worker_pool
                .write()
                .await
                .assign_job(worker_id, &a.job_id);
            if let Some(record) = a.dispatch_record.take() {
                let _ = self
                    .state
                    .board_events
                    .send(crate::BoardEvent::JobDispatched {
                        organization: record.organization.into(),
                        worker_id: worker_id.to_owned(),
                        kind: i16::from(record.kind),
                        score: record.score,
                        build_id: record.derivation_build.map(Into::into),
                        evaluation_id: record.evaluation_id.into(),
                    });
                let state = Arc::clone(&self.state);
                let worker = worker_id.to_owned();
                self.state.shutdown.spawn(async move {
                    persist_dispatched_job(&state, &worker, record).await;
                });
            }
        }
        assignment
    }
}

/// Persist a `dispatched_job` row, open the `build_attempt`, and stamp the
/// anchor's `dispatched_at`. Best-effort: failures are logged so instrumentation
/// can't break dispatch.
async fn persist_dispatched_job(state: &Arc<ServerState>, worker_id: &str, rec: DispatchRecord) {
    let now = now();
    let dispatched_job_id = gradient_entity::ids::DispatchedJobId::now_v7();
    let row = gradient_entity::dispatched_job::Model {
        id: dispatched_job_id,
        kind: rec.kind,
        evaluation_id: rec.evaluation_id,
        organization: rec.organization,
        project: rec.project,
        worker_id: worker_id.to_owned(),
        score: rec.score,
        queued_at: rec.queued_at,
        ready_at: Some(rec.ready_at),
        dispatched_at: now,
        score_breakdown: rec.score_breakdown,
        worker_context: rec.worker_context,
        job_context: rec.job_context,
        instance_context: Some(rec.instance_context.clone()),
        created_at: now,
        ..Default::default()
    }
    .into_active_model();

    if let Err(e) = gradient_entity::dispatched_job::Entity::insert(row)
        .exec(&state.worker_db)
        .await
    {
        warn!(error = %e, "failed to insert dispatched_job");
    }

    let Some(derivation_build) = rec.derivation_build else {
        return;
    };

    // Find/create the build_job attributing this anchor to the driving eval,
    // then open the attempt keyed on (build_job, anchor).
    if let Some(build_job) =
        find_or_create_build_job(state, rec.evaluation_id, derivation_build).await
        && let Err(e) = gradient_db::open_attempt(
            &state.worker_db,
            build_job,
            derivation_build,
            dispatched_job_id,
            rec.substitute,
            rec.build_context.clone(),
        )
        .await
    {
        warn!(error = %e, "failed to open build_attempt");
    }

    if let Err(e) = gradient_entity::derivation_build::Entity::update_many()
        .col_expr(
            gradient_entity::derivation_build::Column::DispatchedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(gradient_entity::derivation_build::Column::Id.eq(derivation_build))
        .filter(gradient_entity::derivation_build::Column::DispatchedAt.is_null())
        .exec(&state.worker_db)
        .await
    {
        warn!(error = %e, %derivation_build, "failed to stamp anchor dispatched_at");
    }
}

/// The `build_job` for `(evaluation, anchor.derivation)`. `resolve_anchors`
/// normally pre-creates it at eval time; this upserts then selects so dispatch
/// stays correct for any anchor whose build_job is missing.
async fn find_or_create_build_job(
    state: &Arc<ServerState>,
    evaluation: EvaluationId,
    derivation_build: DerivationBuildId,
) -> Option<gradient_entity::ids::BuildJobId> {
    let anchor = match EDerivationBuild::find_by_id(derivation_build)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => {
            warn!(%derivation_build, "anchor missing while opening build_attempt");
            return None;
        }
        Err(e) => {
            warn!(error = %e, %derivation_build, "anchor lookup failed while opening build_attempt");
            return None;
        }
    };

    let existing = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.eq(anchor.derivation))
        .one(&state.worker_db)
        .await;
    match existing {
        Ok(Some(j)) => return Some(j.id),
        Ok(None) => {}
        Err(e) => warn!(error = %e, "build_job lookup failed"),
    }

    let row = gradient_entity::build_job::Model {
        id: gradient_entity::ids::BuildJobId::now_v7(),
        evaluation,
        derivation: anchor.derivation,
        derivation_build,
        score: 0.0,
        score_breakdown: serde_json::Value::Null,
        created_at: now(),
    }
    .into_active_model();
    match gradient_entity::build_job::Entity::insert(row)
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([CBuildJob::Evaluation, CBuildJob::Derivation])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(&state.worker_db)
        .await
    {
        Ok(_) => {}
        Err(e) => warn!(error = %e, "build_job upsert failed"),
    }

    match EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.eq(anchor.derivation))
        .one(&state.worker_db)
        .await
    {
        Ok(j) => j.map(|j| j.id),
        Err(e) => {
            warn!(error = %e, "build_job re-select failed");
            None
        }
    }
}
