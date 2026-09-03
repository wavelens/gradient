/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Scoring and job assignment (`RequestJob`).

use std::sync::Arc;

use sea_orm::EntityTrait;
use sea_orm::{ColumnTrait, IntoActiveModel, QueryFilter};
use tracing::{info, warn};

use gradient_core::ServerState;
use gradient_types::proto::{CandidateScore, JobKind};
use gradient_types::*;

use crate::Scheduler;
use crate::actor::{AssignOutcome, SchedulerMsg};
use crate::dispatch;
use crate::jobs::{Assignment, DispatchRecord};

impl Scheduler {
    // ── Scoring / assignment ──────────────────────────────────────────────────

    /// Claim the best pending job of `kind` for the worker. When the tracker
    /// has nothing for a build request, refresh from the DB once and retry.
    pub async fn request_job(&self, worker_id: &str, kind: JobKind) -> Option<Assignment> {
        let instance = self.instance.load_full();
        match self.try_assign(worker_id, &kind, &instance).await {
            AssignOutcome::Assigned(a) => {
                info!(%worker_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob");
                return Some(a);
            }
            AssignOutcome::AtCapacity => return None,
            AssignOutcome::Nothing => {}
        }

        if matches!(kind, JobKind::Build) {
            if let Err(e) = dispatch::dispatch_ready_builds(self).await {
                warn!(error = %e, "on-demand dispatch_ready_builds failed");
            }
            self.kick_dispatch();
        }

        match self.try_assign(worker_id, &kind, &instance).await {
            AssignOutcome::Assigned(a) => {
                info!(%worker_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob (after DB refresh)");
                Some(a)
            }
            _ => None,
        }
    }

    pub async fn record_scores(&self, worker_id: &str, scores: Vec<CandidateScore>) {
        let worker = worker_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::RecordScores {
                worker,
                scores,
                reply,
            })
            .await;
    }

    pub async fn job_rejected(&self, worker_id: &str, job_id: &str) {
        let worker = worker_id.to_owned();
        let job_id = job_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::Rejected {
                worker,
                job_id,
                reply,
            })
            .await;
    }

    pub async fn project_for_job(&self, job_id: &str) -> Option<ProjectId> {
        self.active_job(job_id).await.map(|j| j.project_id())
    }

    /// One atomic claim in the actor; the board event and the `dispatched_job`
    /// row follow outside it so DB latency never blocks the mailbox.
    async fn try_assign(
        &self,
        worker_id: &str,
        kind: &JobKind,
        instance: &Arc<gradient_score::InstanceContext>,
    ) -> AssignOutcome {
        let worker = worker_id.to_owned();
        let kind = kind.clone();
        let instance = Arc::clone(instance);
        let outcome = match self
            .call(|reply| SchedulerMsg::Assign {
                worker,
                kind,
                instance,
                reply,
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!(error = %e, %worker_id, "RequestJob did not reach the scheduler");
                return AssignOutcome::Nothing;
            }
        };
        if let AssignOutcome::Assigned(a) = &outcome
            && let Some(record) = a.dispatch_record.clone()
        {
            let _ = self
                .state
                .board_events
                .send(crate::BoardEvent::JobDispatched {
                    project: record.project.into(),
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
        outcome
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
        project: rec.project,
        task: rec.task,
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
