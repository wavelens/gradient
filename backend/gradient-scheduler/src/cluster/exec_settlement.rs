/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::scheduling::cluster::Requeue;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_graph::{ClusterNext, ExecNodeEnd, Transition};
use gradient_wire::types::BuildFailureKind;
use tracing::warn;

use super::settlement::{Fate, MemberReport, Resolution};
use crate::Scheduler;
use crate::job_handlers::exec_status::attempt_end_reason;
use crate::jobs::PendingJob;

const NODE_FAILED: &str = "a node of the job failed";
const JOB_ENDED: &str = "the job ended";
const JOB_ABORTED: &str = "the job was aborted";
const WORKER_LOST: &str = "the worker running the node was lost";

pub fn requeue_for(fate: Fate) -> Requeue {
    match fate {
        Fate::Retry => Requeue::WithinBudget,
        Fate::Restart => Requeue::OutsideBudget,
        Fate::Complete | Fate::Fail | Fate::Abort => Requeue::No,
    }
}

pub fn cluster_next(resolution: Resolution) -> ClusterNext {
    if resolution.requeued {
        ClusterNext::Restart {
            consume_retry: resolution.fate == Fate::Restart,
        }
    } else {
        ClusterNext::End
    }
}

fn survivors_end(resolution: Resolution) -> (AttemptEndReason, &'static str) {
    match resolution.fate {
        Fate::Abort => (AttemptEndReason::Aborted, JOB_ABORTED),
        Fate::Complete => (AttemptEndReason::Aborted, JOB_ENDED),
        Fate::Retry | Fate::Restart if resolution.requeued => {
            (AttemptEndReason::Aborted, NODE_FAILED)
        }
        Fate::Retry | Fate::Restart | Fate::Fail => (AttemptEndReason::Failed, NODE_FAILED),
    }
}

pub fn node_ends(
    reports: Vec<MemberReport>,
    survivors: Vec<PendingJob>,
    survivors_end: (AttemptEndReason, &str),
) -> Vec<ExecNodeEnd> {
    let (survivor_reason, why) = survivors_end;
    let reported = reports.into_iter().filter_map(|report| match report {
        MemberReport::Completed { job } => node_end(&job, AttemptEndReason::Completed, None),
        MemberReport::Failed { job, failure } => node_end(
            &job,
            failure_reason(&job, failure.kind),
            Some(failure.error),
        ),
        MemberReport::Lost { job } => {
            node_end(&job, AttemptEndReason::Lost, Some(WORKER_LOST.into()))
        }
        MemberReport::Aborted { job: Some(job) } => {
            node_end(&job, AttemptEndReason::Aborted, Some(why.into()))
        }
        MemberReport::Aborted { job: None } => None,
    });
    let survived = survivors
        .iter()
        .filter_map(|job| node_end(job, survivor_reason, Some(why.into())));
    reported.chain(survived).collect()
}

fn settle_late_node_report_after_cluster_resolution(
    resolution: Resolution,
    report: MemberReport,
) -> (Vec<ExecNodeEnd>, ClusterNext) {
    let nodes = node_ends(vec![report], Vec::new(), survivors_end(resolution));
    let next = match cluster_next(resolution) {
        ClusterNext::Restart { .. } => ClusterNext::Restart {
            consume_retry: false,
        },
        ClusterNext::End => ClusterNext::End,
    };

    (nodes, next)
}

fn failure_reason(job: &PendingJob, kind: BuildFailureKind) -> AttemptEndReason {
    let custom_image = matches!(job, PendingJob::Exec(exec) if exec.image_path.is_some());
    attempt_end_reason(kind, custom_image)
}

fn node_end(
    job: &PendingJob,
    reason: AttemptEndReason,
    message: Option<String>,
) -> Option<ExecNodeEnd> {
    let PendingJob::Exec(exec) = job else {
        return None;
    };
    Some(ExecNodeEnd {
        attempt: exec.assignment.as_ref()?.attempt,
        reason,
        message,
    })
}

impl Scheduler {
    pub async fn apply_exec_cluster(
        &self,
        resolution: Resolution,
        reports: Vec<MemberReport>,
        survivors: Vec<PendingJob>,
    ) {
        let nodes = node_ends(reports, survivors, survivors_end(resolution));
        let next = cluster_next(resolution);
        if let Err(e) = self
            .graph
            .transition(Transition::ExecClusterSettled { nodes, next })
            .await
        {
            warn!(error = %e, ?next, "settling an exec cluster failed");
        }
    }

    pub async fn send_late_report_exec_node_attempt_close_transition(
        &self,
        resolution: Resolution,
        report: MemberReport,
    ) {
        let (nodes, next) = settle_late_node_report_after_cluster_resolution(resolution, report);
        if let Err(e) = self
            .graph
            .transition(Transition::ExecClusterSettled { nodes, next })
            .await
        {
            warn!(error = %e, ?next, "closing the attempt of a late exec node failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::Failure;
    use crate::jobs::ExecAssignment;
    use gradient_types::exec::ExecSpec;
    use gradient_types::ids::ExecJobAttemptId;
    use gradient_wire::types::BuildFailureKind;

    fn resolution(fate: Fate, requeued: bool) -> Resolution {
        Resolution {
            fate,
            requeued,
            split: false,
        }
    }

    fn node(attempt: ExecJobAttemptId) -> PendingJob {
        let mut job = crate::test_jobs::exec_job(ExecSpec::default());
        job.assignment = Some(ExecAssignment {
            exec_job: job.exec_job,
            attempt,
            number: 1,
            retries_cap: 1,
            token_hash: String::new(),
            token_expires_at: gradient_types::now(),
            master_port: Some(29500),
        });
        PendingJob::Exec(Box::new(job))
    }

    #[test]
    fn a_restart_consumes_a_retry_and_a_loss_does_not() {
        assert_eq!(
            cluster_next(resolution(Fate::Restart, true)),
            ClusterNext::Restart {
                consume_retry: true
            }
        );
        assert_eq!(
            cluster_next(resolution(Fate::Retry, true)),
            ClusterNext::Restart {
                consume_retry: false
            }
        );
        assert_eq!(
            cluster_next(resolution(Fate::Retry, false)),
            ClusterNext::End
        );
        assert_eq!(requeue_for(Fate::Restart), Requeue::OutsideBudget);
        assert_eq!(requeue_for(Fate::Fail), Requeue::No);
    }

    #[test]
    fn a_failed_node_ends_with_its_report_and_the_survivor_is_aborted_for_a_restart() {
        let (failed, survivor) = (ExecJobAttemptId::now_v7(), ExecJobAttemptId::now_v7());
        let report = MemberReport::Failed {
            job: node(failed),
            failure: Failure {
                error: "the program exited with 1".into(),
                kind: BuildFailureKind::Permanent,
                missing_paths: Vec::new(),
            },
        };

        let ends = node_ends(
            vec![report],
            vec![node(survivor)],
            survivors_end(resolution(Fate::Restart, true)),
        );

        assert_eq!(
            ends,
            vec![
                ExecNodeEnd {
                    attempt: failed,
                    reason: AttemptEndReason::Failed,
                    message: Some("the program exited with 1".into()),
                },
                ExecNodeEnd {
                    attempt: survivor,
                    reason: AttemptEndReason::Aborted,
                    message: Some(NODE_FAILED.into()),
                },
            ]
        );
    }

    #[test]
    fn a_late_node_report_closes_its_attempt_without_spending_another_retry() {
        let late = ExecJobAttemptId::now_v7();

        let (ends, next) = settle_late_node_report_after_cluster_resolution(
            resolution(Fate::Restart, true),
            MemberReport::Completed { job: node(late) },
        );

        assert_eq!(
            ends,
            vec![ExecNodeEnd {
                attempt: late,
                reason: AttemptEndReason::Completed,
                message: None,
            }]
        );
        assert_eq!(
            next,
            ClusterNext::Restart {
                consume_retry: false
            }
        );
        let (_, ended) = settle_late_node_report_after_cluster_resolution(
            resolution(Fate::Fail, false),
            MemberReport::Completed { job: node(late) },
        );
        assert_eq!(ended, ClusterNext::End);
    }

    #[test]
    fn survivors_of_a_spent_budget_end_failed() {
        assert_eq!(
            survivors_end(resolution(Fate::Retry, false)).0,
            AttemptEndReason::Failed
        );
        assert_eq!(
            survivors_end(resolution(Fate::Abort, false)).0,
            AttemptEndReason::Aborted
        );
    }
}
