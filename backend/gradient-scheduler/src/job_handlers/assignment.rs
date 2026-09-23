/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Scoring and job assignment (`RequestJob`).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tracing::{info, warn};

use gradient_core::ServerState;
use gradient_db::ClaimGate;
use gradient_graph::Transition;
use gradient_types::proto::{CandidateScore, JobKind};
use gradient_types::*;

use crate::Scheduler;
use crate::actor::{AssignOutcome, SchedulerMsg};
use crate::jobs::{Assignment, DispatchRecord};

/// How many claims one `RequestJob` makes before it answers with no job: each
/// lost claim drops its job from the tracker, so the next attempt scores what is
/// left.
const CLAIM_ATTEMPTS: usize = 3;

impl Scheduler {
    // ── Scoring / assignment ──────────────────────────────────────────────────

    /// Pick the best pending job of `kind` for the worker in the tracker and
    /// claim it in Postgres. The tracker only proposes: a claim another instance
    /// won, or one whose subject moved since the job was assembled, is dropped and
    /// the next best is tried. Nothing here reads the ready set.
    pub async fn request_job(&self, worker_id: &str, kind: JobKind) -> Option<Assignment> {
        let instance = self.instance.load_full();
        for attempt in 0..CLAIM_ATTEMPTS {
            let a = match self.try_assign(worker_id, &kind, &instance).await {
                AssignOutcome::Assigned(a) => a,
                AssignOutcome::AtCapacity | AssignOutcome::Nothing => return None,
            };

            match claim(&self.state, worker_id, &a.dispatch_record).await {
                Ok(true) => {
                    self.announce_dispatch(worker_id, &a.dispatch_record);
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

    /// A lost claim leaves the tracker. A build's anchor is handed back to the
    /// ready set to be read again: if it only changed relay mode it comes back
    /// assembled for the new one, and if it is out elsewhere the read skips it.
    async fn claim_lost(&self, worker_id: &str, a: &Assignment) {
        info!(%worker_id, job_id = %a.job_id(), "claim lost; dropped from the tracker");
        self.drop_assignment(worker_id, a.job_id()).await;
        if let crate::jobs::PendingJob::Build(b) = &a.pending {
            self.state.ready_set.enter([b.derivation]);
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

    /// The tracker's pick, reserved in this instance only. The `dispatched_job`
    /// row is [`claim`]'s job and the board event [`Self::announce_dispatch`]'s,
    /// both run by the caller, so a lost claim leaves no trace of a hand-out that
    /// never happened.
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

/// Hard ceiling on the awaited transition when the liveness watchdog is off,
/// and the cap the heartbeat-derived budget is clamped to.
const TRANSITION_CEILING_MS: u64 = 60_000;

/// Floor on the awaited transition, below one second so the budget stays
/// strictly inside even the tightest configurable deadline.
const TRANSITION_FLOOR_MS: u64 = 500;

/// How long `claim` waits on the `Dispatched` transition.
///
/// The graph actor answers within 600 s, which is far longer than the session
/// may spend on one frame: the worker's heartbeats queue behind it, and a
/// heartbeat the liveness pass never sees costs the worker its registration,
/// its anchor and every other build it is running. Half the deadline keeps the
/// wait well inside it even when the watchdog is configured tighter than the
/// default; with the watchdog disabled the ceiling still applies, because the
/// graph actor's own timeout is no bound on a session at all.
fn transition_budget(heartbeat_timeout_secs: u64) -> Duration {
    let ms = match heartbeat_timeout_secs {
        0 => TRANSITION_CEILING_MS,
        timeout => {
            (timeout.saturating_mul(1000) / 2).clamp(TRANSITION_FLOOR_MS, TRANSITION_CEILING_MS)
        }
    };

    Duration::from_millis(ms)
}

/// One of the job-context counts, as the column takes it. An absent or null key
/// stays `None`: the metrics average must not read "this dispatch missed nothing"
/// off a job that never reported.
fn window_count(job_context: &serde_json::Value, key: &str) -> Option<i32> {
    job_context[key]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
}

/// Claim the job by writing its `dispatched_job` row, then for a build open the
/// `build_attempt` and stamp the anchor's `dispatched_at` through the graph actor;
/// `Ok(false)` when the claim was lost. Awaited before the assignment goes back
/// to the session: the row is the only proof the job is out, so a worker's first
/// report can never precede it, and an error here withdraws the claim instead of
/// letting the job run unrecorded. The mirror holds too: a failed transition
/// closes the row it just wrote, so a withdrawn claim never leaves an open row
/// parking the job. What the withdrawal cannot undo is a transition that merely
/// ran late: `Dispatched` stamps `dispatched_at` once and only once, so a claim
/// dropped on the budget can still spend it and leave the anchor's real dispatch
/// untimestamped.
async fn claim(
    state: &Arc<ServerState>,
    worker_id: &str,
    rec: &DispatchRecord,
) -> anyhow::Result<bool> {
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
        // Read off the same value that becomes the jsonb, so the columns the
        // metrics windows average and the context the board renders cannot
        // disagree about what this dispatch cost.
        missing_nar_size: rec.job_context["missing_nar_size"].as_i64(),
        missing_count: window_count(&rec.job_context, "missing_count"),
        dependency_count: window_count(&rec.job_context, "dependency_count"),
        ..Default::default()
    };

    let won = gradient_db::claim_dispatch(&state.worker_db, row, claim_gate(rec))
        .await
        .context("dispatched_job claim")?;
    let Some(derivation_build) = rec.derivation_build.filter(|_| won) else {
        return Ok(won);
    };

    let budget = transition_budget(state.config.proto.worker_heartbeat_timeout_secs);
    let moved = match tokio::time::timeout(
        budget,
        state.graph.transition(Transition::Dispatched {
            evaluation: rec.evaluation_id,
            anchor: derivation_build,
            dispatched_job: rec.dispatch,
            substitute: rec.substitute,
            build_context: rec.build_context.clone(),
        }),
    )
    .await
    {
        Ok(moved) => moved.context("Dispatched transition"),
        Err(_) => Err(anyhow::anyhow!(
            "Dispatched transition exceeded {}s",
            budget.as_secs()
        )),
    };

    if moved.is_err()
        && let Err(e) = gradient_db::abandon_open_dispatch(&state.worker_db, rec.dispatch).await
    {
        warn!(error = %e, dispatch = %rec.dispatch, "dispatch row left open after a failed transition");
    }

    moved.map(|_| true)
}

/// What the claim re-reads: a build goes out only while its anchor is `Queued`
/// in the relay mode it was assembled for, because an upstream probe that lands
/// in between turns a build into a relay and the stale build would rebuild bytes
/// the upstream already has (#593).
fn claim_gate(rec: &DispatchRecord) -> ClaimGate {
    match rec.derivation_build {
        Some(anchor) => ClaimGate::Build {
            anchor,
            substitute: rec.substitute,
        },
        None => ClaimGate::Eval {
            evaluation: rec.evaluation_id,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session handles one frame at a time, so the wait on the graph actor
    /// is also how long the worker's next heartbeat goes unread. Every
    /// configurable deadline must therefore outlast the budget, or a slow graph
    /// actor unregisters a healthy worker mid-assignment.
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

    /// A disabled watchdog is not a licence to hold the session for the graph
    /// actor's ten minutes, and a budget of zero would withdraw every claim.
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
