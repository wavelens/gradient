/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::cluster_attempt::ClusterAttemptOutcome;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_types::ids::ClusterAttemptId;
use gradient_wire::types::BuildFailureKind;

use super::coordinator::AttemptMember;
use crate::jobs::PendingJob;

#[derive(Debug, Clone)]
pub struct Failure {
    pub error: String,
    pub kind: BuildFailureKind,
    pub missing_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum MemberReport {
    Completed { job: PendingJob },
    Failed { job: PendingJob, failure: Failure },
    Lost { job: PendingJob },
    Aborted { job: Option<PendingJob> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberOutcome {
    Succeeded,
    Failed,
    Lost,
    Aborted,
}

impl MemberReport {
    pub fn from_failure(job: PendingJob, failure: Failure) -> Self {
        match failure.kind {
            BuildFailureKind::Canceled => Self::Lost { job },
            _ => Self::Failed { job, failure },
        }
    }

    pub fn outcome(&self) -> MemberOutcome {
        match self {
            Self::Completed { .. } => MemberOutcome::Succeeded,
            Self::Failed { failure, .. } if failure.kind == BuildFailureKind::Aborted => {
                MemberOutcome::Aborted
            }
            Self::Failed { .. } => MemberOutcome::Failed,
            Self::Lost { .. } => MemberOutcome::Lost,
            Self::Aborted { .. } => MemberOutcome::Aborted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    Complete,
    Retry,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub fate: Fate,
    pub requeued: bool,
}

#[derive(Debug)]
pub enum Disposition {
    Complete(PendingJob),
    Fail(PendingJob, Failure),
    Requeue(PendingJob),
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survivor {
    pub worker: String,
    pub job_id: String,
}

#[derive(Debug)]
pub enum Recorded {
    NotMember(MemberReport),
    Held,
    Preparing {
        attempt: ClusterAttemptId,
        worker: String,
        report: MemberReport,
    },
    Decided(ClusterAttemptId, Fate),
    Deferred,
    Late(Resolution, MemberReport),
}

pub fn verdict(members: &[AttemptMember]) -> Option<Fate> {
    let outcome = |m: &AttemptMember| m.report.as_ref().map(MemberReport::outcome);
    let succeeded = |m: &AttemptMember| outcome(m) == Some(MemberOutcome::Succeeded);
    if members.iter().any(|m| m.primary && succeeded(m)) || members.iter().all(succeeded) {
        return Some(Fate::Complete);
    }
    if members
        .iter()
        .any(|m| outcome(m) == Some(MemberOutcome::Aborted))
    {
        return Some(Fate::Abort);
    }
    members
        .iter()
        .any(|m| {
            matches!(
                outcome(m),
                Some(MemberOutcome::Failed | MemberOutcome::Lost)
            )
        })
        .then_some(Fate::Retry)
}

pub fn dispose(resolution: Resolution, report: MemberReport) -> Disposition {
    if resolution.requeued {
        return match report {
            MemberReport::Completed { job }
            | MemberReport::Failed { job, .. }
            | MemberReport::Lost { job }
            | MemberReport::Aborted { job: Some(job) } => Disposition::Requeue(job),
            MemberReport::Aborted { job: None } => Disposition::Nothing,
        };
    }

    match (resolution.fate, report) {
        (_, MemberReport::Aborted { job: None }) => Disposition::Nothing,
        (Fate::Complete, MemberReport::Completed { job }) => Disposition::Complete(job),
        (Fate::Complete, MemberReport::Failed { job, .. } | MemberReport::Lost { job })
        | (Fate::Complete, MemberReport::Aborted { job: Some(job) }) => Disposition::Fail(
            job,
            aborted("its cluster completed through the primary member"),
        ),
        (_, MemberReport::Completed { job }) => Disposition::Complete(job),
        (_, MemberReport::Failed { job, failure }) => Disposition::Fail(job, terminal(failure)),
        (_, MemberReport::Lost { job }) => Disposition::Fail(
            job,
            Failure {
                error: "worker lost while running a cluster member; retry budget spent".into(),
                kind: BuildFailureKind::Permanent,
                missing_paths: Vec::new(),
            },
        ),
        (_, MemberReport::Aborted { job: Some(job) }) => {
            Disposition::Fail(job, aborted("a member of its cluster failed"))
        }
    }
}

fn aborted(error: &str) -> Failure {
    Failure {
        error: error.into(),
        kind: BuildFailureKind::Aborted,
        missing_paths: Vec::new(),
    }
}

/// A kind the graph is answering with a requeue would dispatch the member alone.
fn terminal(failure: Failure) -> Failure {
    match failure.kind {
        BuildFailureKind::Transient
        | BuildFailureKind::SubstituteUnavailable
        | BuildFailureKind::InputsUnavailable
        | BuildFailureKind::CorruptEvalCache => Failure {
            error: format!("cluster retry budget spent: {}", failure.error),
            kind: BuildFailureKind::Permanent,
            ..failure
        },
        _ => failure,
    }
}

pub fn attempt_outcome(fate: Fate) -> ClusterAttemptOutcome {
    match fate {
        Fate::Complete => ClusterAttemptOutcome::Succeeded,
        Fate::Retry => ClusterAttemptOutcome::Failed,
        Fate::Abort => ClusterAttemptOutcome::Aborted,
    }
}

pub fn final_status(fate: Fate) -> ClusterJobStatus {
    match fate {
        Fate::Complete => ClusterJobStatus::Completed,
        Fate::Retry => ClusterJobStatus::Failed,
        Fate::Abort => ClusterJobStatus::Aborted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{AttemptBook, AttemptState};
    use crate::scheduler_tests::eval_job;
    use gradient_types::ids::{ClusterJobId, ProjectId};
    use std::time::Instant;

    fn book_with<const N: usize>(
        attempt: ClusterAttemptId,
        members: [AttemptMember; N],
    ) -> AttemptBook {
        let cluster = ClusterJobId::now_v7();
        let mut book = AttemptBook::default();
        book.open(
            attempt,
            AttemptState {
                cluster,
                parked: crate::cluster::PendingCluster {
                    id: cluster,
                    same_zone: false,
                    queued_at: gradient_types::now(),
                    expected: 0,
                    members: Vec::new(),
                    not_before: None,
                },
                members: members.into(),
                roster: Vec::new(),
                deadline: Instant::now(),
                started: true,
                all_accepted: false,
                verdict: None,
                resolving: false,
                resolution: None,
                resolved_at: None,
            },
        );
        book
    }

    fn job() -> PendingJob {
        PendingJob::Eval(eval_job(ProjectId::now_v7()))
    }

    fn member(job_id: &str, primary: bool, report: Option<MemberReport>) -> AttemptMember {
        AttemptMember {
            job_id: job_id.into(),
            worker: format!("w-{job_id}"),
            role: "r".into(),
            index: 0,
            primary,
            accepted: true,
            report,
            settled: false,
        }
    }

    fn failed(kind: BuildFailureKind) -> MemberReport {
        MemberReport::Failed {
            job: job(),
            failure: Failure {
                error: "boom".into(),
                kind,
                missing_paths: Vec::new(),
            },
        }
    }

    fn completed() -> MemberReport {
        MemberReport::Completed { job: job() }
    }

    #[test]
    fn an_undecided_attempt_waits() {
        let members = [
            member("a", false, Some(completed())),
            member("b", false, None),
        ];

        assert_eq!(verdict(&members), None);
    }

    #[test]
    fn the_primary_completes_the_cluster_and_aborts_its_companions() {
        let members = [
            member("p", true, Some(completed())),
            member("c", false, None),
        ];
        let resolution = Resolution {
            fate: Fate::Complete,
            requeued: false,
        };

        assert_eq!(verdict(&members), Some(Fate::Complete));
        let Disposition::Fail(_, failure) =
            dispose(resolution, MemberReport::Aborted { job: Some(job()) })
        else {
            panic!("a companion is failed as aborted");
        };
        assert_eq!(failure.kind, BuildFailureKind::Aborted);
    }

    #[test]
    fn a_completed_member_is_requeued_when_a_sibling_fails() {
        let members = [
            member("a", false, Some(completed())),
            member("b", false, Some(failed(BuildFailureKind::Permanent))),
        ];
        let resolution = Resolution {
            fate: Fate::Retry,
            requeued: true,
        };

        assert_eq!(verdict(&members), Some(Fate::Retry));
        assert!(matches!(
            dispose(resolution, completed()),
            Disposition::Requeue(_)
        ));
        assert!(matches!(
            dispose(resolution, failed(BuildFailureKind::Permanent)),
            Disposition::Requeue(_)
        ));
    }

    #[test]
    fn a_spent_budget_turns_requeueing_kinds_permanent() {
        let resolution = Resolution {
            fate: Fate::Retry,
            requeued: false,
        };
        for kind in [
            BuildFailureKind::Transient,
            BuildFailureKind::SubstituteUnavailable,
            BuildFailureKind::InputsUnavailable,
            BuildFailureKind::CorruptEvalCache,
        ] {
            let Disposition::Fail(_, failure) = dispose(resolution, failed(kind)) else {
                panic!("{kind:?} fails the member");
            };
            assert_eq!(failure.kind, BuildFailureKind::Permanent, "{kind:?}");
        }
        assert!(matches!(
            dispose(resolution, completed()),
            Disposition::Complete(_)
        ));
    }

    #[test]
    fn a_canceled_member_retries_its_cluster_like_a_lost_one() {
        let canceled = MemberReport::from_failure(
            job(),
            Failure {
                error: "the worker stopped before the job finished".into(),
                kind: BuildFailureKind::Canceled,
                missing_paths: Vec::new(),
            },
        );
        assert_eq!(canceled.outcome(), MemberOutcome::Lost);

        let members = [member("a", false, Some(canceled)), member("b", false, None)];
        assert_eq!(verdict(&members), Some(Fate::Retry));
    }

    #[test]
    fn an_aborted_member_aborts_the_cluster_without_retry() {
        let members = [
            member("a", false, Some(failed(BuildFailureKind::Aborted))),
            member("b", false, None),
        ];

        assert_eq!(verdict(&members), Some(Fate::Abort));
    }

    #[test]
    fn a_report_racing_the_resolution_is_disposed_late() {
        let attempt = ClusterAttemptId::now_v7();
        let mut book = book_with(
            attempt,
            [member("a", false, None), member("b", false, None)],
        );

        assert!(matches!(
            book.record("a", failed(BuildFailureKind::Permanent)),
            Recorded::Decided(_, Fate::Retry)
        ));
        let resolution = Resolution {
            fate: Fate::Retry,
            requeued: true,
        };
        let (reports, survivors) = book.drain(attempt, resolution);
        assert_eq!(reports.len(), 1);
        assert_eq!(survivors.len(), 1);

        assert!(matches!(book.record("b", completed()), Recorded::Late(r, _) if r == resolution));
        assert_eq!(book.attempt_of("b"), None);
    }

    #[test]
    fn the_entry_outlives_an_unreleased_survivor() {
        let attempt = ClusterAttemptId::now_v7();
        let mut book = book_with(
            attempt,
            [member("a", false, None), member("b", false, None)],
        );
        book.record("a", failed(BuildFailureKind::Permanent));
        let (_, survivors) = book.drain(
            attempt,
            Resolution {
                fate: Fate::Retry,
                requeued: true,
            },
        );

        assert_eq!(survivors[0].job_id, "b");
        assert_eq!(book.attempt_of("b"), Some(attempt));
        book.settle(attempt, "b");
        assert_eq!(book.attempt_of("b"), None);
    }

    #[test]
    fn an_unstarted_attempt_hands_a_failure_to_the_prepare_path() {
        let attempt = ClusterAttemptId::now_v7();
        let mut book = book_with(
            attempt,
            [member("a", false, None), member("b", false, None)],
        );
        book.get_mut(attempt).expect("open").started = false;

        assert!(matches!(
            book.record("a", MemberReport::Lost { job: job() }),
            Recorded::Preparing { worker, .. } if worker == "w-a"
        ));
    }
}
