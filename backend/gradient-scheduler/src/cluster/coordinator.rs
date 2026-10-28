/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gradient_types::ids::{ClusterAttemptId, ClusterJobId, DispatchedJobId};
use gradient_util::sync::Mutex;
use gradient_wire::types::{ClusterAddress, ClusterPeer};

use super::PendingCluster;
use super::settlement::{
    ExecCluster, Fate, MemberOutcome, MemberReport, Recorded, Resolution, Survivor, exec_verdict,
    verdict,
};

pub const CLUSTER_HOLD_MARGIN_SECS: u32 = 10;
pub const CLUSTER_RETRY_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct AttemptMember {
    pub job_id: String,
    pub assignment_id: DispatchedJobId,
    pub worker: String,
    pub role: String,
    pub index: u32,
    pub primary: bool,
    pub accepted: bool,
    pub report: Option<MemberReport>,
    pub settled: bool,
}

impl AttemptMember {
    fn address(&self) -> ClusterAddress {
        ClusterAddress {
            role: self.role.clone(),
            index: self.index,
        }
    }
}

#[derive(Debug)]
pub struct AttemptState {
    pub cluster: ClusterJobId,
    pub parked: PendingCluster,
    pub members: Vec<AttemptMember>,
    pub roster: Vec<ClusterPeer>,
    pub deadline: Instant,
    pub started: bool,
    pub all_accepted: bool,
    pub verdict: Option<Fate>,
    pub resolving: bool,
    pub resolution: Option<Resolution>,
    pub resolved_at: Option<Instant>,
    pub split: bool,
    pub planned: bool,
    pub exec: Option<ExecCluster>,
}

impl AttemptState {
    fn decide(&self) -> Option<Fate> {
        match &self.exec {
            Some(cluster) => exec_verdict(&self.members, cluster),
            None => verdict(&self.members, self.split && self.planned),
        }
    }
}

#[derive(Debug)]
pub enum Acceptance {
    NotMember,
    Waiting,
    AllAccepted {
        cluster: ClusterJobId,
        attempt: ClusterAttemptId,
        workers: Vec<String>,
        roster: Vec<ClusterPeer>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct SignalRoute {
    pub from: ClusterAddress,
    pub workers: Vec<String>,
}

#[derive(Debug, Default)]
struct Attempts {
    attempts: HashMap<ClusterAttemptId, AttemptState>,
    by_job: HashMap<String, ClusterAttemptId>,
}

impl Attempts {
    pub fn open(&mut self, attempt: ClusterAttemptId, state: AttemptState) {
        for m in &state.members {
            self.by_job.insert(m.job_id.clone(), attempt);
        }
        self.attempts.insert(attempt, state);
    }

    pub fn cluster_of(&self, attempt: ClusterAttemptId) -> Option<ClusterJobId> {
        self.attempts.get(&attempt).map(|s| s.cluster)
    }

    pub fn attempt_of(&self, job_id: &str) -> Option<ClusterAttemptId> {
        self.by_job.get(job_id).copied()
    }

    pub fn accept(&mut self, job_id: &str, endpoint: Option<String>) -> Acceptance {
        let Some(attempt) = self.attempt_of(job_id) else {
            return Acceptance::NotMember;
        };
        let Some(state) = self.attempts.get_mut(&attempt) else {
            return Acceptance::NotMember;
        };
        if let Some(m) = state.members.iter_mut().find(|m| m.job_id == job_id) {
            m.accepted = true;
            let address = m.address();
            if let Some(peer) = state
                .roster
                .iter_mut()
                .find(|p| p.role == address.role && p.index == address.index)
                && endpoint.is_some()
            {
                peer.endpoint = endpoint;
            }
        }
        if state.all_accepted || !state.members.iter().all(|m| m.accepted) {
            return Acceptance::Waiting;
        }

        state.all_accepted = true;
        Acceptance::AllAccepted {
            cluster: state.cluster,
            attempt,
            workers: state.members.iter().map(|m| m.worker.clone()).collect(),
            roster: state.roster.clone(),
        }
    }

    pub fn mark_started(&mut self, attempt: ClusterAttemptId) -> Option<Option<Fate>> {
        let state = self.attempts.get_mut(&attempt)?;
        state.started = true;
        if state.verdict.is_some() {
            return Some(None);
        }
        state.verdict = state.decide();

        Some(state.verdict)
    }

    pub fn preparing(&self, job_id: &str) -> Option<ClusterAttemptId> {
        let attempt = self.attempt_of(job_id)?;
        self.attempts
            .get(&attempt)
            .filter(|s| !s.started)
            .map(|_| attempt)
    }

    pub fn take(&mut self, attempt: ClusterAttemptId) -> Option<AttemptState> {
        let state = self.attempts.remove(&attempt)?;
        for m in &state.members {
            self.by_job.remove(&m.job_id);
        }

        Some(state)
    }

    pub fn overdue(&self, now: Instant) -> Vec<ClusterAttemptId> {
        self.attempts
            .iter()
            .filter(|(_, s)| !s.started && !s.all_accepted && s.deadline <= now)
            .map(|(a, _)| *a)
            .collect()
    }

    pub fn route(
        &self,
        attempt: ClusterAttemptId,
        sender: &str,
        to: Option<&ClusterAddress>,
    ) -> Option<SignalRoute> {
        let state = self.attempts.get(&attempt).filter(|s| s.started)?;
        let from = state.members.iter().find(|m| m.worker == sender)?.address();
        let workers = state
            .members
            .iter()
            .filter(|m| m.worker != sender)
            .filter(|m| to.is_none_or(|to| m.address() == *to))
            .map(|m| m.worker.clone())
            .collect();

        Some(SignalRoute { from, workers })
    }

    pub fn record(&mut self, job_id: &str, report: MemberReport, planned: bool) -> Recorded {
        let Some(&attempt) = self.by_job.get(job_id) else {
            return Recorded::NotMember(report);
        };
        let Some(state) = self.attempts.get_mut(&attempt) else {
            return Recorded::NotMember(report);
        };
        if let Some(resolution) = state.resolution {
            self.settle(attempt, job_id);
            return Recorded::Late(resolution, report);
        }
        let Some(member) = state.members.iter_mut().find(|m| m.job_id == job_id) else {
            return Recorded::NotMember(report);
        };
        if !state.started && !state.all_accepted && report.outcome() != MemberOutcome::Aborted {
            return Recorded::Preparing {
                attempt,
                worker: member.worker.clone(),
                assignment_id: member.assignment_id,
                report,
            };
        }
        member.report = Some(report);
        state.planned |= planned;
        if !state.started {
            return Recorded::Held;
        }
        if state.verdict.is_some() {
            return Recorded::Deferred;
        }
        match state.decide() {
            Some(fate) => {
                state.verdict = Some(fate);
                Recorded::Decided(attempt, fate)
            }
            None => Recorded::Held,
        }
    }

    pub fn drain(
        &mut self,
        attempt: ClusterAttemptId,
        resolution: Resolution,
    ) -> (Vec<MemberReport>, Vec<Survivor>) {
        let Some(state) = self.attempts.get_mut(&attempt) else {
            return (Vec::new(), Vec::new());
        };
        state.resolution = Some(resolution);
        state.resolved_at = Some(Instant::now());
        let mut reports = Vec::new();
        let mut survivors = Vec::new();
        for member in &mut state.members {
            match member.report.take() {
                Some(report) => {
                    member.settled = true;
                    reports.push(report);
                }
                None => survivors.push(Survivor {
                    worker: member.worker.clone(),
                    job_id: member.job_id.clone(),
                }),
            }
        }
        self.forget_if_settled(attempt);

        (reports, survivors)
    }

    pub fn settle(&mut self, attempt: ClusterAttemptId, job_id: &str) {
        if let Some(member) = self
            .attempts
            .get_mut(&attempt)
            .and_then(|s| s.members.iter_mut().find(|m| m.job_id == job_id))
        {
            member.settled = true;
        }
        self.forget_if_settled(attempt);
    }

    fn forget_if_settled(&mut self, attempt: ClusterAttemptId) {
        if self
            .attempts
            .get(&attempt)
            .is_some_and(|s| s.members.iter().all(|m| m.settled))
            && let Some(state) = self.attempts.remove(&attempt)
        {
            for member in state.members {
                self.by_job.remove(&member.job_id);
            }
        }
    }

    pub fn split_member(&self, job_id: &str) -> bool {
        self.attempt_of(job_id)
            .and_then(|attempt| self.attempts.get(&attempt))
            .is_some_and(|s| s.split)
    }

    pub fn pending_verdicts(&self) -> Vec<(ClusterAttemptId, Fate)> {
        self.attempts
            .iter()
            .filter(|(_, s)| s.resolution.is_none() && !s.resolving)
            .filter_map(|(a, s)| s.verdict.map(|fate| (*a, fate)))
            .collect()
    }

    pub fn begin_resolving(&mut self, attempt: ClusterAttemptId) -> Option<ClusterJobId> {
        let state = self
            .attempts
            .get_mut(&attempt)
            .filter(|s| !s.resolving && s.resolution.is_none())?;
        state.resolving = true;

        Some(state.cluster)
    }

    pub fn abort_resolving(&mut self, attempt: ClusterAttemptId) {
        if let Some(state) = self.attempts.get_mut(&attempt) {
            state.resolving = false;
        }
    }

    pub fn expire_resolved(&mut self, now: Instant, max_age: Duration) {
        let stale: Vec<ClusterAttemptId> = self
            .attempts
            .iter()
            .filter(|(_, s)| {
                s.resolved_at
                    .is_some_and(|at| now.duration_since(at) >= max_age)
            })
            .map(|(a, _)| *a)
            .collect();
        for attempt in stale {
            self.take(attempt);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AttemptBook(Arc<Mutex<Attempts>>);

impl AttemptBook {
    pub async fn open(&self, attempt: ClusterAttemptId, state: AttemptState) {
        self.0.lock().open(attempt, state);
    }

    pub async fn cluster_of(&self, attempt: ClusterAttemptId) -> Option<ClusterJobId> {
        self.0.lock().cluster_of(attempt)
    }

    pub async fn attempt_of(&self, job_id: &str) -> Option<ClusterAttemptId> {
        self.0.lock().attempt_of(job_id)
    }

    pub async fn accept(&self, job_id: &str, endpoint: Option<String>) -> Acceptance {
        self.0.lock().accept(job_id, endpoint)
    }

    pub async fn mark_started(&self, attempt: ClusterAttemptId) -> Option<Option<Fate>> {
        self.0.lock().mark_started(attempt)
    }

    pub async fn preparing(&self, job_id: &str) -> Option<ClusterAttemptId> {
        self.0.lock().preparing(job_id)
    }

    pub async fn take(&self, attempt: ClusterAttemptId) -> Option<AttemptState> {
        self.0.lock().take(attempt)
    }

    pub async fn overdue(&self, now: Instant) -> Vec<ClusterAttemptId> {
        self.0.lock().overdue(now)
    }

    pub async fn route(
        &self,
        attempt: ClusterAttemptId,
        sender: &str,
        to: Option<&ClusterAddress>,
    ) -> Option<SignalRoute> {
        self.0.lock().route(attempt, sender, to)
    }

    pub async fn record(&self, job_id: &str, report: MemberReport, planned: bool) -> Recorded {
        self.0.lock().record(job_id, report, planned)
    }

    pub async fn split_member(&self, job_id: &str) -> bool {
        self.0.lock().split_member(job_id)
    }

    pub async fn split(&self, attempt: ClusterAttemptId) -> bool {
        self.0
            .lock()
            .attempts
            .get(&attempt)
            .is_some_and(|s| s.split)
    }

    pub async fn is_exec(&self, attempt: ClusterAttemptId) -> bool {
        self.0
            .lock()
            .attempts
            .get(&attempt)
            .is_some_and(|s| s.exec.is_some())
    }

    pub async fn drain(
        &self,
        attempt: ClusterAttemptId,
        resolution: Resolution,
    ) -> (Vec<MemberReport>, Vec<Survivor>) {
        self.0.lock().drain(attempt, resolution)
    }

    pub async fn settle(&self, attempt: ClusterAttemptId, job_id: &str) {
        self.0.lock().settle(attempt, job_id);
    }

    pub async fn pending_verdicts(&self) -> Vec<(ClusterAttemptId, Fate)> {
        self.0.lock().pending_verdicts()
    }

    pub async fn begin_resolving(&self, attempt: ClusterAttemptId) -> Option<ClusterJobId> {
        self.0.lock().begin_resolving(attempt)
    }

    pub async fn abort_resolving(&self, attempt: ClusterAttemptId) {
        self.0.lock().abort_resolving(attempt);
    }

    pub async fn expire_resolved(&self, now: Instant, max_age: Duration) {
        self.0.lock().expire_resolved(now, max_age);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::ClusterJobId;

    fn member(job: &str, worker: &str, role: &str, index: u32) -> AttemptMember {
        AttemptMember {
            job_id: job.into(),
            assignment_id: DispatchedJobId::now_v7(),
            worker: worker.into(),
            role: role.into(),
            index,
            primary: false,
            accepted: false,
            report: None,
            settled: false,
        }
    }

    fn book(deadline: Instant) -> (Attempts, ClusterAttemptId) {
        let cluster = ClusterJobId::now_v7();
        let attempt = ClusterAttemptId::now_v7();
        let mut book = Attempts::default();
        book.open(
            attempt,
            AttemptState {
                cluster,
                parked: PendingCluster {
                    id: cluster,
                    same_zone: false,
                    queued_at: gradient_types::now(),
                    startable_since: None,
                    expected: 0,
                    members: Vec::new(),
                    not_before: None,
                },
                members: vec![
                    member("build:a", "w1", "server", 0),
                    member("build:b", "w2", "client", 0),
                ],
                roster: Vec::new(),
                deadline,
                started: false,
                all_accepted: false,
                verdict: None,
                resolving: false,
                resolution: None,
                resolved_at: None,
                split: false,
                planned: false,
                exec: None,
            },
        );

        (book, attempt)
    }

    #[test]
    fn the_last_acceptance_starts_the_attempt() {
        let (mut book, attempt) = book(Instant::now() + Duration::from_secs(30));

        assert!(matches!(book.accept("build:a", None), Acceptance::Waiting));
        let Acceptance::AllAccepted {
            attempt: started,
            workers,
            ..
        } = book.accept("build:b", None)
        else {
            panic!("all accepted");
        };
        assert_eq!(started, attempt);
        assert_eq!(workers, vec!["w1".to_owned(), "w2".to_owned()]);
        assert!(matches!(book.accept("eval:x", None), Acceptance::NotMember));
    }

    #[test]
    fn only_an_unstarted_attempt_past_its_deadline_is_overdue() {
        let now = Instant::now();
        let (mut book, attempt) = book(now);

        assert_eq!(book.overdue(now + Duration::from_secs(1)), vec![attempt]);
        let _ = book.mark_started(attempt);
        assert!(book.overdue(now + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn an_attempt_every_member_accepted_is_never_overdue() {
        let now = Instant::now();
        let (mut book, _) = book(now);
        book.accept("build:a", None);
        book.accept("build:b", None);

        assert!(book.overdue(now + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn a_failed_attempt_fails_once() {
        let (mut book, attempt) = book(Instant::now());

        assert!(book.take(attempt).is_some());
        assert!(book.take(attempt).is_none());
        assert_eq!(book.attempt_of("build:a"), None);
    }

    #[test]
    fn signals_route_only_within_a_started_attempt() {
        let (mut book, attempt) = book(Instant::now());
        assert!(book.route(attempt, "w1", None).is_none());

        let _ = book.mark_started(attempt);
        let broadcast = book.route(attempt, "w1", None).expect("member");
        assert_eq!(broadcast.workers, vec!["w2".to_owned()]);
        assert_eq!(
            broadcast.from,
            ClusterAddress {
                role: "server".into(),
                index: 0
            }
        );
        let direct = ClusterAddress {
            role: "server".into(),
            index: 0,
        };
        assert_eq!(
            book.route(attempt, "w2", Some(&direct))
                .expect("member")
                .workers,
            vec!["w1".to_owned()]
        );
        assert!(book.route(attempt, "w9", None).is_none());
        assert!(book.route(ClusterAttemptId::now_v7(), "w1", None).is_none());
    }

    #[test]
    fn a_report_while_the_start_commits_waits_for_it() {
        let (mut book, attempt) = book(Instant::now());
        book.accept("build:a", None);
        book.accept("build:b", None);
        let lost = MemberReport::Aborted { job: None };

        assert!(matches!(
            book.record("build:a", lost, false),
            Recorded::Held
        ));
        assert_eq!(book.mark_started(attempt), Some(Some(Fate::Abort)));
    }

    #[test]
    fn a_failed_resolution_is_retried() {
        let (mut book, attempt) = book(Instant::now());
        let _ = book.mark_started(attempt);
        book.record("build:a", MemberReport::Aborted { job: None }, false);
        assert!(book.begin_resolving(attempt).is_some());
        assert!(book.pending_verdicts().is_empty(), "underway");

        book.abort_resolving(attempt);

        assert_eq!(book.pending_verdicts(), vec![(attempt, Fate::Abort)]);
    }
}
