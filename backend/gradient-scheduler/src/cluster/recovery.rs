/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_pool::session_port::SessionSignal;
use gradient_types::ids::{ClusterAttemptId, DispatchedJobId};
use tracing::warn;

use super::settlement::*;
use crate::Scheduler;
use crate::actor::{Released, SchedulerMsg};
use crate::jobs::PendingJob;

impl Scheduler {
    pub(crate) async fn on_cluster_member_closed(
        &self,
        job_id: &str,
        report: MemberReport,
    ) -> Result<()> {
        let recorded = self.cluster.book.record(job_id, report).await;
        match recorded {
            Recorded::Held | Recorded::Deferred => Ok(()),
            Recorded::NotMember(report) => self.settle_single(report).await,
            Recorded::Preparing {
                attempt,
                worker,
                assignment_id,
                report,
            } => {
                if let MemberReport::Completed { job }
                | MemberReport::Failed { job, .. }
                | MemberReport::Lost { job } = report
                {
                    self.return_member(&worker, job_id, assignment_id, job, attempt)
                        .await;
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
        let Some(cluster) = self.cluster.book.begin_resolving(attempt).await else {
            return Ok(());
        };
        let resolved = gradient_db::scheduling::cluster::resolve_cluster_attempt(
            &self.db.worker_db,
            cluster,
            attempt,
            fate.into(),
            fate == Fate::Retry,
            fate.into(),
        )
        .await;
        let requeued = match resolved {
            Ok(Some(requeued)) => requeued,
            Ok(None) => {
                warn!(%attempt, "cluster attempt already closed; settling its members without a retry");
                false
            }
            Err(e) => {
                self.cluster.book.abort_resolving(attempt).await;
                return Err(e.into());
            }
        };

        let resolution = Resolution { fate, requeued };
        let (reports, survivors) = self.cluster.book.drain(attempt, resolution).await;
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
        self.build_kick.kick();

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
                self.cluster.book.settle(attempt, &survivor.job_id).await;
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
                Disposition::Fail(job, failure) => self.settle_failed(job, &failure, None).await,
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
        crate::build::requeue_cluster_members(&self.db, &self.graph, &requeue).await;
    }

    async fn settle_single(&self, report: MemberReport) -> Result<()> {
        match report {
            MemberReport::Completed { job } => self.settle_completed(job, false).await,
            MemberReport::Failed { job, failure } => self.settle_failed(job, &failure, None).await,
            MemberReport::Lost { job } => {
                crate::build::requeue_orphaned_jobs(
                    &self.db,
                    &self.graph,
                    &[job],
                    &Default::default(),
                )
                .await;
                Ok(())
            }
            MemberReport::Aborted { .. } => Ok(()),
        }
    }

    pub(crate) async fn return_member(
        &self,
        worker: &str,
        key: &str,
        assignment_id: DispatchedJobId,
        job: PendingJob,
        attempt: ClusterAttemptId,
    ) {
        let (worker, key) = (worker.to_owned(), key.to_owned());
        let _ = self
            .call(|reply| SchedulerMsg::ReturnMember {
                worker,
                key,
                assignment_id,
                job,
                attempt,
                reply,
            })
            .await;
    }
}
