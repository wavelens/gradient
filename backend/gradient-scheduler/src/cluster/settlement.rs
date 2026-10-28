/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::cluster_attempt::ClusterAttemptOutcome;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::ids::{ClusterAttemptId, DispatchedJobId};
use gradient_wire::types::BuildFailureKind;

use super::coordinator::AttemptMember;
use crate::job_handlers::exec_status::attempt_end_reason;
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
        let lost = match &job {
            PendingJob::Exec(exec) => {
                attempt_end_reason(failure.kind, exec.image_path.is_some())
                    == AttemptEndReason::Lost
            }
            PendingJob::Eval(_) | PendingJob::Build(_) => {
                failure.kind == BuildFailureKind::Canceled
            }
        };
        if lost {
            Self::Lost { job }
        } else {
            Self::Failed { job, failure }
        }
    }

    pub fn job(&self) -> Option<&PendingJob> {
        match self {
            Self::Completed { job } | Self::Failed { job, .. } | Self::Lost { job } => Some(job),
            Self::Aborted { job } => job.as_ref(),
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
    Restart,
    Fail,
    Abort,
}

impl From<Fate> for ClusterJobStatus {
    fn from(fate: Fate) -> Self {
        match fate {
            Fate::Complete => ClusterJobStatus::Completed,
            Fate::Retry | Fate::Restart | Fate::Fail => ClusterJobStatus::Failed,
            Fate::Abort => ClusterJobStatus::Aborted,
        }
    }
}

impl From<Fate> for ClusterAttemptOutcome {
    fn from(fate: Fate) -> Self {
        match fate {
            Fate::Complete => ClusterAttemptOutcome::Succeeded,
            Fate::Retry | Fate::Restart | Fate::Fail => ClusterAttemptOutcome::Failed,
            Fate::Abort => ClusterAttemptOutcome::Aborted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecCluster {
    pub keep_running: bool,
    pub retries_left: u32,
}

impl ExecCluster {
    pub fn of<'a>(jobs: impl IntoIterator<Item = &'a PendingJob>) -> Option<Self> {
        let mut cluster: Option<Self> = None;
        for job in jobs {
            let PendingJob::Exec(exec) = job else {
                return None;
            };
            let cap = exec.assignment.as_ref().map_or(0, |a| a.retries_cap);
            let retries_left = exec.retries_left.min(cap);
            cluster = Some(Self {
                keep_running: exec.spec.keep_running,
                retries_left: cluster.map_or(retries_left, |c| c.retries_left.min(retries_left)),
            });
        }

        cluster
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub fate: Fate,
    pub requeued: bool,
    pub split: bool,
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
        assignment_id: DispatchedJobId,
        report: MemberReport,
    },
    Decided(ClusterAttemptId, Fate),
    Deferred,
    Late(Resolution, MemberReport),
}

pub fn verdict(members: &[AttemptMember], planned_split: bool) -> Option<Fate> {
    if planned_split {
        return split_verdict(members);
    }

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

pub fn exec_verdict(members: &[AttemptMember], cluster: &ExecCluster) -> Option<Fate> {
    let outcomes: Vec<Option<MemberOutcome>> = members
        .iter()
        .map(|m| m.report.as_ref().map(MemberReport::outcome))
        .collect();
    let any = |outcome: MemberOutcome| outcomes.contains(&Some(outcome));
    if outcomes
        .iter()
        .all(|o| *o == Some(MemberOutcome::Succeeded))
    {
        return Some(Fate::Complete);
    }
    if any(MemberOutcome::Aborted) {
        return Some(Fate::Abort);
    }
    if any(MemberOutcome::Lost) {
        return Some(Fate::Retry);
    }
    if !any(MemberOutcome::Failed) {
        return None;
    }

    match (cluster.keep_running, cluster.retries_left) {
        (true, _) if outcomes.contains(&None) => None,
        (true, _) | (false, 0) => Some(Fate::Fail),
        (false, _) => Some(Fate::Restart),
    }
}

fn split_verdict(members: &[AttemptMember]) -> Option<Fate> {
    let outcomes: Vec<Option<MemberOutcome>> = members
        .iter()
        .map(|m| m.report.as_ref().map(MemberReport::outcome))
        .collect();
    let aborted = outcomes.contains(&Some(MemberOutcome::Aborted));
    if outcomes.contains(&None) {
        return aborted.then_some(Fate::Abort);
    }
    if outcomes.contains(&Some(MemberOutcome::Succeeded)) {
        return Some(Fate::Complete);
    }

    Some(if aborted { Fate::Abort } else { Fate::Retry })
}

pub fn dispose(resolution: Resolution, report: MemberReport) -> Disposition {
    if resolution.split && resolution.fate == Fate::Complete {
        return Disposition::Nothing;
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{AttemptBook, AttemptState};
    use crate::test_jobs::eval_job;
    use gradient_types::ids::{ClusterJobId, ProjectId};
    use std::time::Instant;

    async fn book_with<const N: usize>(
        attempt: ClusterAttemptId,
        started: bool,
        members: [AttemptMember; N],
    ) -> AttemptBook {
        let cluster = ClusterJobId::now_v7();
        let book = AttemptBook::default();
        book.open(
            attempt,
            AttemptState {
                cluster,
                parked: crate::cluster::PendingCluster {
                    id: cluster,
                    same_zone: false,
                    queued_at: gradient_types::now(),
                    startable_since: None,
                    expected: 0,
                    members: Vec::new(),
                    not_before: None,
                },
                members: members.into(),
                roster: Vec::new(),
                deadline: Instant::now(),
                started,
                all_accepted: false,
                verdict: None,
                resolving: false,
                resolution: None,
                resolved_at: None,
                split: false,
                planned: false,
                exec: None,
            },
        )
        .await;
        book
    }

    fn job() -> PendingJob {
        PendingJob::Eval(eval_job(ProjectId::now_v7()))
    }

    fn member(job_id: &str, primary: bool, report: Option<MemberReport>) -> AttemptMember {
        AttemptMember {
            job_id: job_id.into(),
            assignment_id: DispatchedJobId::now_v7(),
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

    fn exec_member_job() -> PendingJob {
        PendingJob::Exec(Box::new(crate::test_jobs::exec_job(
            gradient_types::exec::ExecSpec::default(),
        )))
    }

    #[test]
    fn one_failed_node_restarts_the_cluster_while_retries_are_left() {
        let members = [
            member("a", false, None),
            member("b", false, Some(failed(BuildFailureKind::Permanent))),
        ];
        let cluster = |retries_left| ExecCluster {
            keep_running: false,
            retries_left,
        };
        assert_eq!(exec_verdict(&members, &cluster(1)), Some(Fate::Restart));
        assert_eq!(exec_verdict(&members, &cluster(0)), Some(Fate::Fail));
    }

    #[test]
    fn a_timeout_counts_as_a_failure() {
        let members = [
            member("a", false, None),
            member("b", false, Some(failed(BuildFailureKind::Timeout))),
        ];
        let cluster = ExecCluster {
            keep_running: false,
            retries_left: 2,
        };
        assert_eq!(exec_verdict(&members, &cluster), Some(Fate::Restart));
    }

    #[test]
    fn keep_running_lets_the_others_finish_then_fails() {
        let cluster = ExecCluster {
            keep_running: true,
            retries_left: 2,
        };
        let running = [
            member("a", false, None),
            member("b", false, Some(failed(BuildFailureKind::Permanent))),
        ];
        assert_eq!(exec_verdict(&running, &cluster), None);
        let done = [
            member("a", false, Some(completed())),
            member("b", false, Some(failed(BuildFailureKind::Permanent))),
        ];
        assert_eq!(exec_verdict(&done, &cluster), Some(Fate::Fail));
    }

    #[test]
    fn a_loss_after_a_kept_failure_restarts_the_cluster() {
        let members = [
            member(
                "a",
                false,
                Some(MemberReport::Lost {
                    job: exec_member_job(),
                }),
            ),
            member("b", false, Some(failed(BuildFailureKind::Permanent))),
        ];
        let cluster = ExecCluster {
            keep_running: true,
            retries_left: 0,
        };
        assert_eq!(exec_verdict(&members, &cluster), Some(Fate::Retry));
    }

    #[test]
    fn an_abort_wins_and_every_node_must_succeed() {
        let cluster = ExecCluster {
            keep_running: false,
            retries_left: 3,
        };
        let aborted = [
            member("a", false, Some(completed())),
            member("b", false, Some(MemberReport::Aborted { job: None })),
        ];
        assert_eq!(exec_verdict(&aborted, &cluster), Some(Fate::Abort));
        let running = [
            member("a", false, Some(completed())),
            member("b", false, None),
        ];
        assert_eq!(exec_verdict(&running, &cluster), None);
        let done = [
            member("a", false, Some(completed())),
            member("b", false, Some(completed())),
        ];
        assert_eq!(exec_verdict(&done, &cluster), Some(Fate::Complete));
    }

    #[test]
    fn a_guest_boot_failure_of_a_exec_node_is_a_loss() {
        let report = MemberReport::from_failure(
            exec_member_job(),
            Failure {
                error: "no agent".into(),
                kind: BuildFailureKind::GuestBoot,
                missing_paths: vec![],
            },
        );
        assert_eq!(report.outcome(), MemberOutcome::Lost);
    }

    #[test]
    fn an_undecided_attempt_waits() {
        let members = [
            member("a", false, Some(completed())),
            member("b", false, None),
        ];

        assert_eq!(verdict(&members, false), None);
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
            split: false,
        };

        assert_eq!(verdict(&members, false), Some(Fate::Complete));
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
            split: false,
        };

        assert_eq!(verdict(&members, false), Some(Fate::Retry));
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
            split: false,
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
        assert_eq!(verdict(&members, false), Some(Fate::Retry));
    }

    #[test]
    fn the_primary_lost_after_planning_does_not_retry_the_attempt() {
        let lost = MemberReport::Lost { job: job() };
        let members = [member("p", true, Some(lost)), member("c", false, None)];

        assert_eq!(verdict(&members, true), None);
    }

    #[test]
    fn the_primary_lost_before_planning_retries_the_attempt() {
        let lost = MemberReport::Lost { job: job() };
        let members = [member("p", true, Some(lost)), member("c", false, None)];

        assert_eq!(verdict(&members, false), Some(Fate::Retry));
    }

    #[test]
    fn a_planned_split_completes_with_any_member_and_retries_only_when_all_are_lost() {
        let lost = || MemberReport::Lost { job: job() };
        let finished = [
            member("p", true, Some(lost())),
            member("c", false, Some(completed())),
        ];
        let all_lost = [
            member("p", true, Some(lost())),
            member("c", false, Some(failed(BuildFailureKind::Permanent))),
        ];

        assert_eq!(verdict(&finished, true), Some(Fate::Complete));
        assert_eq!(verdict(&all_lost, true), Some(Fate::Retry));
        let resolution = Resolution {
            fate: Fate::Complete,
            requeued: false,
            split: true,
        };
        assert!(matches!(
            dispose(resolution, MemberReport::Aborted { job: Some(job()) }),
            Disposition::Nothing
        ));
    }

    #[test]
    fn every_member_report_is_folded_once_the_attempt_settles() {
        let running = [
            member("p", true, Some(completed())),
            member("c", false, None),
        ];
        let settled = [
            member("p", true, Some(completed())),
            member("c", false, Some(completed())),
        ];
        let aborted = [
            member("p", true, Some(failed(BuildFailureKind::Aborted))),
            member("c", false, None),
        ];

        assert_eq!(verdict(&running, true), None);
        assert_eq!(verdict(&settled, true), Some(Fate::Complete));
        assert_eq!(verdict(&aborted, true), Some(Fate::Abort));
    }

    #[test]
    fn an_aborted_member_aborts_the_cluster_without_retry() {
        let members = [
            member("a", false, Some(failed(BuildFailureKind::Aborted))),
            member("b", false, None),
        ];

        assert_eq!(verdict(&members, false), Some(Fate::Abort));
    }

    #[tokio::test]
    async fn a_report_racing_the_resolution_is_disposed_late() {
        let attempt = ClusterAttemptId::now_v7();
        let book = book_with(
            attempt,
            true,
            [member("a", false, None), member("b", false, None)],
        )
        .await;

        assert!(matches!(
            book.record("a", failed(BuildFailureKind::Permanent), false)
                .await,
            Recorded::Decided(_, Fate::Retry)
        ));
        let resolution = Resolution {
            fate: Fate::Retry,
            requeued: true,
            split: false,
        };
        let (reports, survivors) = book.drain(attempt, resolution).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(survivors.len(), 1);

        assert!(
            matches!(book.record("b", completed(), false).await, Recorded::Late(r, _) if r == resolution)
        );
        assert_eq!(book.attempt_of("b").await, None);
    }

    #[tokio::test]
    async fn the_entry_outlives_an_unreleased_survivor() {
        let attempt = ClusterAttemptId::now_v7();
        let book = book_with(
            attempt,
            true,
            [member("a", false, None), member("b", false, None)],
        )
        .await;
        book.record("a", failed(BuildFailureKind::Permanent), false)
            .await;
        let (_, survivors) = book
            .drain(
                attempt,
                Resolution {
                    fate: Fate::Retry,
                    requeued: true,
                    split: false,
                },
            )
            .await;

        assert_eq!(survivors[0].job_id, "b");
        assert_eq!(book.attempt_of("b").await, Some(attempt));
        book.settle(attempt, "b").await;
        assert_eq!(book.attempt_of("b").await, None);
    }

    #[tokio::test]
    async fn an_unstarted_attempt_hands_a_failure_to_the_prepare_path() {
        let attempt = ClusterAttemptId::now_v7();
        let book = book_with(
            attempt,
            false,
            [member("a", false, None), member("b", false, None)],
        )
        .await;

        assert!(matches!(
            book.record("a", MemberReport::Lost { job: job() }, false).await,
            Recorded::Preparing { worker, .. } if worker == "w-a"
        ));
    }
}
