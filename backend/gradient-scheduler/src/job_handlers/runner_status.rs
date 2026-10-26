/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::runner_jobs::attempts;
use gradient_entity::runner_job_attempt::AttemptEndReason;
use gradient_graph::{Graph, Transition};
use gradient_types::ids::{RunnerJobAttemptId, RunnerJobId};
use gradient_types::{ERunnerJob, MRunnerJob};
use gradient_wire::types::BuildFailureKind;
use sea_orm::EntityTrait;
use tracing::{info, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::jobs::{PendingRunnerJob, runner_job_key};

const WORKER_LOST: &str = "the worker running the job was lost";
const WORKER_REJECTED: &str = "the worker rejected the job";

pub(crate) fn attempt_end_reason(kind: BuildFailureKind) -> AttemptEndReason {
    match kind {
        BuildFailureKind::Permanent => AttemptEndReason::Failed,
        BuildFailureKind::Timeout => AttemptEndReason::Timeout,
        BuildFailureKind::Aborted => AttemptEndReason::Aborted,
        BuildFailureKind::Transient
        | BuildFailureKind::Canceled
        | BuildFailureKind::GuestBoot
        | BuildFailureKind::SubstituteUnavailable
        | BuildFailureKind::InputsUnavailable
        | BuildFailureKind::CorruptEvalCache => AttemptEndReason::Lost,
    }
}

pub(crate) async fn end_attempt(
    graph: &Graph,
    job: &PendingRunnerJob,
    reason: AttemptEndReason,
    message: Option<String>,
) -> Result<()> {
    let Some(assignment) = job.assignment else {
        warn!(runner_job = %job.runner_job, "a runner report without an attempt; ignored");
        return Ok(());
    };
    graph
        .transition(Transition::RunnerAttemptEnded {
            attempt: assignment.attempt,
            reason,
            message,
        })
        .await?;
    Ok(())
}

pub(crate) async fn lose_open_attempt(db: &DbContext, graph: &Graph, runner_job: RunnerJobId) {
    let open = match attempts::open_of_job(&db.worker_db, runner_job).await {
        Ok(open) => open,
        Err(e) => {
            warn!(error = %e, %runner_job, "reading the open attempt of a lost runner job failed");
            return;
        }
    };
    let Some(open) = open else {
        return;
    };
    let ended = graph
        .transition(Transition::RunnerAttemptEnded {
            attempt: open.id,
            reason: AttemptEndReason::Lost,
            message: Some(WORKER_LOST.to_owned()),
        })
        .await;
    if let Err(e) = ended {
        warn!(error = %e, %runner_job, "ending the attempt of a lost runner job failed");
    }
}

impl Scheduler {
    pub async fn active_runner_attempt(
        &self,
        worker_id: &str,
        job_id: &str,
    ) -> Option<RunnerJobAttemptId> {
        let (worker, job_id) = (worker_id.to_owned(), job_id.to_owned());
        self.call(|reply| SchedulerMsg::ActiveRunnerAttempt {
            worker,
            job_id,
            reply,
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn handle_runner_started(&self, worker_id: &str, job_id: &str) {
        let Some(attempt) = self.active_runner_attempt(worker_id, job_id).await else {
            return;
        };
        if let Err(e) = self
            .graph
            .transition(Transition::RunnerProgramStarted { attempt })
            .await
        {
            warn!(error = %e, %worker_id, %job_id, "recording a runner program start failed");
        }
    }

    pub async fn handle_runner_phase(
        &self,
        worker_id: &str,
        job_id: &str,
        name: String,
        payload: Vec<u8>,
    ) {
        let Some(attempt) = self.active_runner_attempt(worker_id, job_id).await else {
            return;
        };
        if let Err(e) = self
            .graph
            .transition(Transition::RunnerPhase {
                attempt,
                name,
                payload,
            })
            .await
        {
            warn!(error = %e, %worker_id, %job_id, "recording a runner phase failed");
        }
    }

    pub async fn abort_runner_job(&self, job: RunnerJobId) -> Result<bool> {
        let Some(row): Option<MRunnerJob> =
            ERunnerJob::find_by_id(job).one(&self.db.worker_db).await?
        else {
            return Ok(false);
        };
        let report = self
            .graph
            .transition(Transition::AbortRunnerJob { runner_job: job })
            .await?;
        if report.aborted_runner_jobs.is_empty() {
            return Ok(false);
        }

        self.cancel_jobs(row.evaluation, vec![runner_job_key(job)])
            .await;
        Ok(true)
    }

    pub(crate) async fn runner_rejected(&self, worker_id: &str, job_id: &str) {
        let attempt = self.active_runner_attempt(worker_id, job_id).await;
        self.drop_assignment(worker_id, job_id).await;
        let Some(attempt) = attempt else {
            return;
        };
        info!(%worker_id, %job_id, "runner job rejected; its attempt ends as lost");
        if let Err(e) = self
            .graph
            .transition(Transition::RunnerAttemptEnded {
                attempt,
                reason: AttemptEndReason::Lost,
                message: Some(WORKER_REJECTED.to_owned()),
            })
            .await
        {
            warn!(error = %e, %worker_id, %job_id, "ending a rejected runner attempt failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_end_reason_maps_every_failure_kind() {
        use BuildFailureKind::*;
        assert_eq!(attempt_end_reason(Permanent), AttemptEndReason::Failed);
        assert_eq!(attempt_end_reason(Timeout), AttemptEndReason::Timeout);
        assert_eq!(attempt_end_reason(Aborted), AttemptEndReason::Aborted);
        for lost in [
            Transient,
            Canceled,
            GuestBoot,
            SubstituteUnavailable,
            InputsUnavailable,
            CorruptEvalCache,
        ] {
            assert_eq!(attempt_end_reason(lost), AttemptEndReason::Lost, "{lost:?}");
        }
    }
}
