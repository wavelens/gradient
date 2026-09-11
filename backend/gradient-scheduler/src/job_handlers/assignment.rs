/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Scoring and job assignment (`RequestJob`).

use std::sync::Arc;

use anyhow::Context as _;
use sea_orm::EntityTrait;
use sea_orm::IntoActiveModel;
use tracing::{info, warn};

use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_graph::Transition;
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
        for attempt in 0..3 {
            match self.try_assign(worker_id, &kind, &instance).await {
                AssignOutcome::Assigned(a) if self.still_queued(&a).await => {
                    if let Err(e) =
                        record_dispatch(&self.state, worker_id, &a.dispatch_record).await
                    {
                        warn!(error = %e, %worker_id, job_id = %a.job_id, "dispatch record not written; assignment withdrawn");
                        self.job_rejected(worker_id, &a.job_id).await;
                        return None;
                    }

                    self.announce_dispatch(worker_id, &a.dispatch_record);
                    info!(%worker_id, job_id = %a.job_id, ?kind, attempt, "job assigned via RequestJob");
                    return Some(a);
                }
                AssignOutcome::Assigned(a) => {
                    self.drop_assignment(worker_id, &a.job_id).await;
                    continue;
                }
                AssignOutcome::AtCapacity => return None,
                AssignOutcome::Nothing => {}
            }

            if attempt == 0 && matches!(kind, JobKind::Build) {
                if let Err(e) = dispatch::dispatch_ready_builds(self).await {
                    warn!(error = %e, "on-demand dispatch_ready_builds failed");
                }
                self.kick_dispatch();
            } else {
                return None;
            }
        }

        None
    }

    /// `Queued` is an invariant the counters keep; a job enqueued before a
    /// gate regressed is still in the tracker and must not go out.
    async fn still_queued(&self, a: &Assignment) -> bool {
        let Some(anchor) = a.pending.derivation_build() else {
            return true;
        };

        match gradient_db::anchor_status(&self.state.worker_db, anchor).await {
            Ok(Some(BuildStatus::Queued)) => true,
            Ok(status) => {
                warn!(job_id = %a.job_id, ?status, "queued job no longer dispatchable; dropped from the tracker");
                false
            }
            Err(e) => {
                warn!(job_id = %a.job_id, error = %e, "anchor status lookup failed; dispatching anyway");
                true
            }
        }
    }

    async fn drop_assignment(&self, worker_id: &str, job_id: &str) {
        let worker = worker_id.to_owned();
        let job_id = job_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::Release {
                worker,
                job_id,
                reply,
            })
            .await;
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

    /// One atomic claim in the actor. The `dispatched_job` row is
    /// [`record_dispatch`]'s job and the board event
    /// [`Self::announce_dispatch`]'s, both run by the caller once the claim
    /// survives its re-check, so a vetoed claim leaves no trace of a hand-out
    /// that never happened.
    async fn try_assign(
        &self,
        worker_id: &str,
        kind: &JobKind,
        instance: &Arc<gradient_score::InstanceContext>,
    ) -> AssignOutcome {
        let worker = worker_id.to_owned();
        let kind = kind.clone();
        let instance = Arc::clone(instance);
        match self
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
                AssignOutcome::Nothing
            }
        }
    }

    /// Announce the hand-out to the job board. Fired only once the record is
    /// durable, so the board never shows a job that was withdrawn.
    fn announce_dispatch(&self, worker_id: &str, record: &DispatchRecord) {
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
    }
}

/// The `dispatched_job` row, then for a build the open `build_attempt` and the
/// anchor's `dispatched_at` through the graph actor. Awaited before the
/// assignment goes back to the session: the row is the only proof the job is
/// out, so a worker's first report can never precede it, and an error here
/// withdraws the claim instead of letting the job run unrecorded.
async fn record_dispatch(
    state: &Arc<ServerState>,
    worker_id: &str,
    rec: &DispatchRecord,
) -> anyhow::Result<()> {
    let now = now();
    let row = gradient_entity::dispatched_job::Model {
        id: rec.dispatch,
        kind: rec.kind,
        evaluation_id: rec.evaluation_id,
        project: rec.project,
        task: rec.task,
        worker_id: worker_id.to_owned(),
        job_id: Some(rec.job_id.clone()),
        score: rec.score,
        queued_at: rec.queued_at,
        ready_at: Some(rec.ready_at),
        dispatched_at: now,
        score_breakdown: rec.score_breakdown.clone(),
        worker_context: rec.worker_context.clone(),
        job_context: rec.job_context.clone(),
        instance_context: Some(rec.instance_context.clone()),
        created_at: now,
        ..Default::default()
    }
    .into_active_model();

    gradient_entity::dispatched_job::Entity::insert(row)
        .exec_without_returning(&state.worker_db)
        .await
        .context("dispatched_job insert")?;

    let Some(derivation_build) = rec.derivation_build else {
        return Ok(());
    };

    state
        .graph
        .transition(Transition::Dispatched {
            evaluation: rec.evaluation_id,
            anchor: derivation_build,
            dispatched_job: rec.dispatch,
            substitute: rec.substitute,
            build_context: rec.build_context.clone(),
        })
        .await
        .context("Dispatched transition")?;

    Ok(())
}
