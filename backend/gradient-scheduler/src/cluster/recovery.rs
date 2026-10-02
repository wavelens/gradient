/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_pool::session_port::SessionSignal;
use gradient_types::ids::ClusterAttemptId;
use tracing::warn;

use super::settlement::*;
use crate::Scheduler;
use crate::actor::{Released, SchedulerMsg};
use crate::jobs::PendingJob;

impl Scheduler {
    pub(crate) fn attempt_of(&self, job_id: &str) -> Option<ClusterAttemptId> {
        self.attempts.lock().attempt_of(job_id)
    }

    pub(crate) async fn on_cluster_member_closed(
        &self,
        job_id: &str,
        report: MemberReport,
    ) -> Result<()> {
        let recorded = self.attempts.lock().record(job_id, report);
        match recorded {
            Recorded::Held | Recorded::Deferred => Ok(()),
            Recorded::NotMember(report) => self.settle_single(report).await,
            Recorded::Preparing {
                attempt,
                worker,
                report,
            } => {
                if let MemberReport::Completed { job }
                | MemberReport::Failed { job, .. }
                | MemberReport::Lost { job } = report
                {
                    self.return_member(&worker, job_id, job, attempt).await;
                }
                self.fail_prepare(attempt).await;
                Ok(())
            }
            Recorded::Decided(attempt, fate) => self.resolve_attempt(attempt, fate).await,
            Recorded::Late(resolution, report) => {
                self.apply(vec![dispose(resolution, report)]).await;
                Ok(())
            }
        }
    }

    /// A failed database write is leaving the verdict in the book. The cluster-dispatch pass is
    /// retrying it.
    pub(crate) async fn resolve_attempt(
        &self,
        attempt: ClusterAttemptId,
        fate: Fate,
    ) -> Result<()> {
        let Some(cluster) = self.attempts.lock().begin_resolving(attempt) else {
            return Ok(());
        };
        let resolved = gradient_db::scheduling::cluster::resolve_cluster_attempt(
            &self.state.worker_db,
            cluster,
            attempt,
            attempt_outcome(fate),
            fate == Fate::Retry,
            final_status(fate),
        )
        .await;
        let requeued = match resolved {
            Ok(Some(requeued)) => requeued,
            Ok(None) => {
                warn!(%attempt, "cluster attempt already closed; settling its members without a retry");
                false
            }
            Err(e) => {
                self.attempts.lock().abort_resolving(attempt);
                return Err(e.into());
            }
        };

        let resolution = Resolution { fate, requeued };
        let (reports, survivors) = self.attempts.lock().drain(attempt, resolution);
        let mut dispositions: Vec<Disposition> = reports
            .into_iter()
            .map(|r| dispose(resolution, r))
            .collect();
        for job in self.abort_survivors(attempt, &survivors).await {
            dispositions.push(dispose(
                resolution,
                MemberReport::Aborted { job: Some(job) },
            ));
        }
        self.apply(dispositions).await;
        self.kick_assigner();

        Ok(())
    }

    async fn abort_survivors(
        &self,
        attempt: ClusterAttemptId,
        survivors: &[Survivor],
    ) -> Vec<PendingJob> {
        let mut workers: Vec<String> = survivors.iter().map(|s| s.worker.clone()).collect();
        workers.sort();
        workers.dedup();
        let signals = workers
            .into_iter()
            .map(|w| {
                let abort = SessionSignal::AbortCluster {
                    attempt: attempt.to_string(),
                    reason: "cluster attempt settled".into(),
                };
                (w, abort)
            })
            .collect();
        self.signal_workers(signals).await;

        let mut jobs = Vec::new();
        for survivor in survivors {
            let (worker, job_id) = (survivor.worker.clone(), survivor.job_id.clone());
            let released = self
                .call(|reply| SchedulerMsg::Release {
                    worker,
                    job_id,
                    reply,
                })
                .await;
            if let Ok(Released { job: Some(job), .. }) = released {
                self.attempts.lock().settle(attempt, &survivor.job_id);
                jobs.push(job);
            }
        }

        jobs
    }

    async fn apply(&self, dispositions: Vec<Disposition>) {
        let mut requeue = Vec::new();
        for disposition in dispositions {
            let settled = match disposition {
                Disposition::Complete(job) => self.settle_completed(job, false).await,
                Disposition::Fail(job, failure) => self.settle_failed(job, &failure).await,
                Disposition::Requeue(job) => {
                    requeue.push(job);
                    Ok(())
                }
                Disposition::Nothing => Ok(()),
            };
            if let Err(e) = settled {
                warn!(error = %e, "settling a cluster member failed");
            }
        }
        crate::build::requeue_cluster_members(&self.state, &requeue).await;
    }

    async fn settle_single(&self, report: MemberReport) -> Result<()> {
        match report {
            MemberReport::Completed { job } => self.settle_completed(job, false).await,
            MemberReport::Failed { job, failure } => self.settle_failed(job, &failure).await,
            MemberReport::Lost { job } => {
                crate::build::requeue_orphaned_jobs(&self.state, &[job]).await;
                Ok(())
            }
            MemberReport::Aborted { .. } => Ok(()),
        }
    }

    pub(crate) async fn return_member(
        &self,
        worker: &str,
        key: &str,
        job: PendingJob,
        attempt: ClusterAttemptId,
    ) {
        let (worker, key) = (worker.to_owned(), key.to_owned());
        let _ = self
            .call(|reply| SchedulerMsg::ReturnMember {
                worker,
                key,
                job,
                attempt,
                reply,
            })
            .await;
    }
}
