/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::worker;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tracing::{Instrument as _, info, warn};

use gradient_db::DbContext;
use gradient_graph::{Graph, Transition};
use gradient_types::*;
use gradient_wire::types::{CandidateScore, JobKind};

use crate::Scheduler;
use crate::actor::{AssignOutcome, SchedulerMsg};
use crate::jobs::{Assignment, AssignmentRecord};

const CLAIM_ATTEMPTS: usize = 3;

impl Scheduler {
    #[tracing::instrument(level = "debug", skip_all, fields(?kind))]
    pub async fn request_job(&self, worker_id: &str, kind: JobKind) -> Option<Assignment> {
        let instance = self.scoring.instance.load_full();
        for attempt in 0..CLAIM_ATTEMPTS {
            let a = match self.try_assign(worker_id, &kind, &instance).await {
                AssignOutcome::Assigned(a) => a,
                AssignOutcome::AtCapacity => return None,
                AssignOutcome::Nothing => {
                    self.cluster.wake.notify_one();
                    return None;
                }
            };

            match claim(&self.db, &self.graph, worker_id, &a.assignment_record)
                .instrument(tracing::debug_span!("claim_dispatch"))
                .await
            {
                Ok(true) => {
                    self.announce_assignment(worker_id, &a.assignment_record);
                    info!(%worker_id, job_id = %a.job_id(), ?kind, attempt, "job assigned via RequestJob");
                    return Some(a);
                }
                Ok(false) => self.claim_lost(worker_id, &a).await,
                Err(e) => {
                    warn!(error = format!("{e:#}"), %worker_id, job_id = %a.job_id(), "dispatch record not written; assignment withdrawn");
                    self.job_rejected(worker_id, a.job_id()).await;
                    return None;
                }
            }
        }

        None
    }

    async fn claim_lost(&self, worker_id: &str, a: &Assignment) {
        info!(%worker_id, job_id = %a.job_id(), "claim lost; dropped from the tracker");
        self.drop_assignment(worker_id, a.job_id()).await;
        if let crate::jobs::PendingJob::Build(b) = &a.pending {
            self.db.memory.startable_set.enter([b.derivation]);
        }
    }

    pub(crate) async fn drop_assignment(&self, worker_id: &str, job_id: &str) {
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

    #[tracing::instrument(level = "debug", skip_all, fields(scores = scores.len()))]
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
        if gradient_db::scheduling::assignment_record::runner_job_of_key(job_id).is_some() {
            return self.runner_rejected(worker_id, job_id).await;
        }
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

    async fn try_assign(
        &self,
        worker_id: &str,
        kind: &JobKind,
        instance: &Arc<gradient_pool::score::InstanceContext>,
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

    pub(crate) fn announce_assignment(&self, worker_id: &str, record: &AssignmentRecord) {
        self.db.events.publish(worker::JobDispatched {
            project: record.project,
            worker_id: worker_id.to_owned(),
            kind: i16::from(record.kind),
            score: record.score,
            build_id: record.derivation_build,
            evaluation_id: record.evaluation_id,
        });
    }
}

const TRANSITION_CEILING_MS: u64 = 60_000;

const TRANSITION_FLOOR_MS: u64 = 500;

fn transition_budget(heartbeat_timeout_secs: u64) -> Duration {
    let ms = match heartbeat_timeout_secs {
        0 => TRANSITION_CEILING_MS,
        timeout => {
            (timeout.saturating_mul(1000) / 2).clamp(TRANSITION_FLOOR_MS, TRANSITION_CEILING_MS)
        }
    };

    Duration::from_millis(ms)
}

fn window_count(job_context: &serde_json::Value, key: &str) -> Option<i32> {
    job_context[key]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
}

pub(crate) fn assignment_row(
    rec: &AssignmentRecord,
    worker_id: &str,
    now: chrono::NaiveDateTime,
) -> gradient_entity::dispatched_job::Model {
    gradient_entity::dispatched_job::Model {
        id: rec.assignment_id,
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
        missing_nar_size: rec.job_context["missing_nar_size"].as_i64(),
        missing_count: window_count(&rec.job_context, "missing_count"),
        dependency_count: window_count(&rec.job_context, "dependency_count"),
        ..Default::default()
    }
}

pub(crate) async fn assigned_transition(
    config: &RuntimeConfig,
    graph: &Graph,
    worker_id: &str,
    rec: &AssignmentRecord,
) -> anyhow::Result<bool> {
    let transition = match (rec.derivation_build, rec.runner) {
        (Some(shared_build), _) => Transition::Assigned { shared_build },
        (None, Some(runner)) => Transition::RunnerJobAssigned {
            runner_job: runner.runner_job,
            attempt: runner.attempt,
            number: runner.number,
            assignment: rec.assignment_id,
            worker: worker_id.to_owned(),
            retries_cap: runner.retries_cap,
        },
        (None, None) => return Ok(true),
    };
    let budget = transition_budget(config.proto.worker_heartbeat_timeout_secs);
    match tokio::time::timeout(budget, graph.transition(transition)).await {
        Ok(moved) => moved
            .map(|report| !report.already_aborted)
            .context("Dispatched transition"),
        Err(_) => Err(anyhow::anyhow!(
            "Dispatched transition exceeded {}s",
            budget.as_secs()
        )),
    }
}

/// The `dispatched_job` row is the only proof the job is out. It must exist before the assignment
/// is handed back to the session, to keep a worker's first report from preceding it. A failed
/// transition is closing the row it just wrote. A claim dropped on the budget can still leave the
/// real dispatch untimestamped, since `Assigned` is stamping `dispatched_at` only once.
async fn claim(
    db: &DbContext,
    graph: &Graph,
    worker_id: &str,
    rec: &AssignmentRecord,
) -> anyhow::Result<bool> {
    let won = gradient_db::scheduling::assignment_record::claim_assignment(
        &db.worker_db,
        assignment_row(rec, worker_id, now()),
        rec.into(),
    )
    .await
    .context("dispatched_job claim")?;
    if !won || (rec.derivation_build.is_none() && rec.runner.is_none()) {
        return Ok(won);
    }

    let moved = assigned_transition(&db.config, graph, worker_id, rec).await;
    if !matches!(moved, Ok(true))
        && let Err(e) = gradient_db::scheduling::assignment_record::abandon_open_assignment(
            &db.worker_db,
            rec.assignment_id,
        )
        .await
    {
        warn!(error = %e, dispatch = %rec.assignment_id, "dispatch row left open after a failed transition");
    }

    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_expires_before_the_liveness_deadline() {
        for timeout in [1_u64, 2, 10, 30, 60, 120, 600, 3600] {
            assert!(
                transition_budget(timeout) < Duration::from_secs(timeout),
                "budget for a {timeout}s deadline: {:?}",
                transition_budget(timeout)
            );
        }
    }

    #[test]
    fn the_budget_is_bounded_at_both_ends() {
        assert_eq!(
            transition_budget(0),
            Duration::from_millis(TRANSITION_CEILING_MS)
        );
        assert_eq!(
            transition_budget(u64::MAX),
            Duration::from_millis(TRANSITION_CEILING_MS)
        );
        assert_eq!(
            transition_budget(1),
            Duration::from_millis(TRANSITION_FLOOR_MS)
        );
    }
}
