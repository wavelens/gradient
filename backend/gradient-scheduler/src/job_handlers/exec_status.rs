/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::exec_jobs::attempts;
use gradient_db::exec_jobs::run_again::RunRefusal;
use gradient_db::scheduling::assignment_record::exec_job_of_key;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_graph::{Graph, Transition};
use gradient_types::ids::{ExecJobAttemptId, ExecJobId};
use gradient_types::{EExecJob, MExecJob};
use gradient_wire::types::BuildFailureKind;
use sea_orm::EntityTrait;
use tracing::{info, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::jobs::{PendingExecJob, exec_job_key};

const WORKER_LOST: &str = "the worker running the job was lost";
const WORKER_REJECTED: &str = "the worker rejected the job";

pub(crate) fn attempt_end_reason(kind: BuildFailureKind, custom_image: bool) -> AttemptEndReason {
    match kind {
        BuildFailureKind::Permanent => AttemptEndReason::Failed,
        BuildFailureKind::GuestBoot if custom_image => AttemptEndReason::Failed,
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
    job: &PendingExecJob,
    reason: AttemptEndReason,
    message: Option<String>,
) -> Result<()> {
    let Some(assignment) = &job.assignment else {
        warn!(exec_job = %job.exec_job, "an exec report without an attempt; ignored");
        return Ok(());
    };
    graph
        .transition(Transition::ExecAttemptEnded {
            attempt: assignment.attempt,
            reason,
            message,
        })
        .await?;
    Ok(())
}

pub(crate) async fn lose_open_attempt(db: &DbContext, graph: &Graph, exec_job: ExecJobId) {
    let open = match attempts::open_of_job(&db.worker_db, exec_job).await {
        Ok(open) => open,
        Err(e) => {
            warn!(error = %e, %exec_job, "reading the open attempt of a lost exec job failed");
            return;
        }
    };
    let Some(open) = open else {
        return;
    };
    let ended = graph
        .transition(Transition::ExecAttemptEnded {
            attempt: open.id,
            reason: AttemptEndReason::Lost,
            message: Some(WORKER_LOST.to_owned()),
        })
        .await;
    if let Err(e) = ended {
        warn!(error = %e, %exec_job, "ending the attempt of a lost exec job failed");
    }
}

impl Scheduler {
    pub async fn active_exec_attempt(
        &self,
        worker_id: &str,
        job_id: &str,
    ) -> Option<ExecJobAttemptId> {
        let (worker, job_id) = (worker_id.to_owned(), job_id.to_owned());
        self.call(|reply| SchedulerMsg::ActiveExecAttempt {
            worker,
            job_id,
            reply,
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn handle_exec_started(&self, worker_id: &str, job_id: &str) {
        let Some(attempt) = self.active_exec_attempt(worker_id, job_id).await else {
            return;
        };
        if let Err(e) = self
            .graph
            .transition(Transition::ExecProgramStarted { attempt })
            .await
        {
            warn!(error = %e, %worker_id, %job_id, "recording an exec program start failed");
        }
        self.resend_keep_vm_running(worker_id, job_id).await;
    }

    pub async fn handle_exec_program_exited(&self, worker_id: &str, job_id: &str, code: i32) {
        let Some(attempt) = self.active_exec_attempt(worker_id, job_id).await else {
            return;
        };
        if let Err(e) = self
            .graph
            .transition(Transition::ExecProgramExited { attempt, code })
            .await
        {
            warn!(error = %e, %worker_id, %job_id, "recording an exec program exit failed");
        }
    }

    pub async fn handle_exec_abort_requested(&self, worker_id: &str, job_id: &str) {
        let attempt = self.active_exec_attempt(worker_id, job_id).await;
        let (Some(_), Some(exec_job)) = (attempt, exec_job_of_key(job_id)) else {
            return;
        };
        info!(%worker_id, %job_id, "the guest shut down; the exec job is aborted");
        if let Err(e) = self.abort_exec_job(exec_job).await {
            warn!(error = %e, %worker_id, %job_id, "aborting an exec job on a guest shutdown failed");
        }
    }

    pub async fn handle_exec_phase(
        &self,
        worker_id: &str,
        job_id: &str,
        name: String,
        payload: Vec<u8>,
    ) {
        let Some(attempt) = self.active_exec_attempt(worker_id, job_id).await else {
            return;
        };
        if let Err(e) = self
            .graph
            .transition(Transition::ExecPhase {
                attempt,
                name,
                payload,
            })
            .await
        {
            warn!(error = %e, %worker_id, %job_id, "recording an exec phase failed");
        }
    }

    pub async fn abort_exec_job(&self, job: ExecJobId) -> Result<bool> {
        let Some(row): Option<MExecJob> = EExecJob::find_by_id(job).one(&self.db.worker_db).await?
        else {
            return Ok(false);
        };
        let report = self
            .graph
            .transition(Transition::AbortExecJob { exec_job: job })
            .await?;
        if report.aborted_exec_jobs.is_empty() {
            return Ok(false);
        }

        let keys = report
            .aborted_exec_jobs
            .iter()
            .copied()
            .map(exec_job_key)
            .collect();
        self.cancel_jobs(row.evaluation, keys).await;
        Ok(true)
    }

    pub async fn run_exec_job(&self, job: ExecJobId) -> Result<Result<(), RunRefusal>> {
        let report = self
            .graph
            .transition(Transition::RunExecJob { exec_job: job })
            .await?;
        Ok(report.exec_run_refusal.map_or(Ok(()), Err))
    }

    pub async fn fail_exec_dispatch(
        &self,
        worker_id: &str,
        job_id: &str,
        message: String,
    ) -> Result<()> {
        self.drop_assignment(worker_id, job_id).await;
        let Some(exec_job) = exec_job_of_key(job_id) else {
            return Ok(());
        };
        self.graph
            .transition(Transition::FailExecJob { exec_job, message })
            .await?;
        Ok(())
    }

    pub(crate) async fn exec_rejected(&self, worker_id: &str, job_id: &str) {
        let attempt = self.active_exec_attempt(worker_id, job_id).await;
        self.drop_assignment(worker_id, job_id).await;
        let Some(attempt) = attempt else {
            return;
        };
        info!(%worker_id, %job_id, "exec job rejected; its attempt ends as lost");
        if let Err(e) = self
            .graph
            .transition(Transition::ExecAttemptEnded {
                attempt,
                reason: AttemptEndReason::Lost,
                message: Some(WORKER_REJECTED.to_owned()),
            })
            .await
        {
            warn!(error = %e, %worker_id, %job_id, "ending a rejected exec attempt failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_end_reason_maps_every_failure_kind() {
        use BuildFailureKind::*;
        assert_eq!(
            attempt_end_reason(Permanent, false),
            AttemptEndReason::Failed
        );
        assert_eq!(
            attempt_end_reason(Timeout, false),
            AttemptEndReason::Timeout
        );
        assert_eq!(
            attempt_end_reason(Aborted, false),
            AttemptEndReason::Aborted
        );
        for lost in [
            Transient,
            Canceled,
            GuestBoot,
            SubstituteUnavailable,
            InputsUnavailable,
            CorruptEvalCache,
        ] {
            assert_eq!(
                attempt_end_reason(lost, false),
                AttemptEndReason::Lost,
                "{lost:?}"
            );
        }
    }

    #[test]
    fn a_custom_image_that_fails_to_boot_consumes_a_retry() {
        use BuildFailureKind::GuestBoot;
        assert_eq!(
            attempt_end_reason(GuestBoot, true),
            AttemptEndReason::Failed
        );
        assert_eq!(attempt_end_reason(GuestBoot, false), AttemptEndReason::Lost);
    }
}
