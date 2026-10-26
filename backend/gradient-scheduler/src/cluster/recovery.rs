/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::eval_reports::drop_eval_reports;
use gradient_db::status::BuildRefusal;
use gradient_graph::Transition;
use gradient_pool::session_port::SessionSignal;
use gradient_types::ids::{ClusterAttemptId, DispatchedJobId};
use gradient_wire::types::BuildFailureKind;
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
        let planned = self.split_member_closed(job_id, &report).await;
        let recorded = self.cluster.book.record(job_id, report, planned).await;
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

        let resolution = Resolution {
            fate,
            requeued,
            split: self.cluster.book.split(attempt).await,
        };
        let (reports, survivors) = self.cluster.book.drain(attempt, resolution).await;
        self.drop_reports_of_a_retry(resolution, &reports).await;
        let completion = split_completion(resolution, &reports);
        let mut dispositions: Vec<Disposition> = reports
            .into_iter()
            .map(|r| dispose(resolution, r))
            .chain(completion)
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

    async fn drop_reports_of_a_retry(&self, resolution: Resolution, reports: &[MemberReport]) {
        if !resolution.split || resolution.fate != Fate::Retry {
            return;
        }
        let Some(evaluation) = reports
            .iter()
            .find_map(|report| report.job())
            .map(PendingJob::evaluation_id)
        else {
            return;
        };

        if let Err(e) = drop_eval_reports(&self.db.worker_db, evaluation).await {
            warn!(%evaluation, error = %e, "dropping the eval stats of a retried split attempt failed");
        }
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
                Disposition::Fail(job, failure) if failure.kind == BuildFailureKind::Aborted => {
                    self.abort_survivor(job, &failure, &mut requeue).await
                }
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

    /// A survivor's build is aborted for its own evaluation, as the build abort of the API does,
    /// and goes back to the queue while another live evaluation still wants it.
    async fn abort_survivor(
        &self,
        job: PendingJob,
        failure: &Failure,
        requeue: &mut Vec<PendingJob>,
    ) -> Result<()> {
        let Some(shared_build) = job.derivation_build() else {
            return self.settle_failed(job, failure, None).await;
        };
        let report = self
            .graph
            .transition(Transition::AbortBuild {
                evaluation: job.evaluation_id(),
                shared_build,
            })
            .await?;
        match report.refusal {
            None => Ok(()),
            Some(BuildRefusal::NeededByOtherEvaluation) => {
                requeue.push(job);
                Ok(())
            }
            Some(_) => self.settle_failed(job, failure, None).await,
        }
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
                    gradient_graph::OrphanCause::WorkerLost,
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

fn split_completion(resolution: Resolution, reports: &[MemberReport]) -> Option<Disposition> {
    if !resolution.split || resolution.fate != Fate::Complete {
        return None;
    }

    reports.iter().find_map(|report| match report {
        MemberReport::Completed { job } => Some(Disposition::Complete(job.clone())),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_jobs::eval_job;
    use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};
    use gradient_test_support::state::TestState;
    use gradient_types::ids::ProjectId;

    fn lost() -> MemberReport {
        MemberReport::Lost {
            job: PendingJob::Eval(eval_job(ProjectId::now_v7())),
        }
    }

    async fn statements_after(fate: Fate, split: bool) -> Vec<sea_orm::Statement> {
        let db = mock_db().append_exec_results([exec_ok()]).into_connection();
        let scheduler = Scheduler::new(TestState::new().worker_db(db.clone()).build());
        let resolution = Resolution {
            fate,
            requeued: true,
            split,
        };

        scheduler
            .drop_reports_of_a_retry(resolution, &[lost()])
            .await;
        raw_statements(db.into_transaction_log())
    }

    #[tokio::test]
    async fn reports_of_a_failed_attempt_are_dropped_on_retry() {
        let dropped = statements_after(Fate::Retry, true).await;

        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(
            dropped[0]
                .sql
                .starts_with("DELETE FROM \"eval_cost_report\""),
            "{}",
            dropped[0].sql
        );
        assert!(statements_after(Fate::Complete, true).await.is_empty());
        assert!(statements_after(Fate::Retry, false).await.is_empty());
    }
}
