/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Open cluster attempts on this instance. `take` is the only way out, so the
//! first of a timeout, a reject or a member's end owns the attempt's close.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gradient_types::ids::{ClusterAttemptId, ClusterJobId};
use gradient_wire::types::{ClusterAddress, ClusterPeer};

use super::PendingCluster;

pub const CLUSTER_HOLD_MARGIN_SECS: u32 = 10;
pub const CLUSTER_RETRY_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberOutcome {
    Succeeded,
    Failed,
    Lost,
}

#[derive(Debug, Clone)]
pub struct AttemptMember {
    pub job_id: String,
    pub worker: String,
    pub role: String,
    pub index: u32,
    pub primary: bool,
    pub accepted: bool,
    pub outcome: Option<MemberOutcome>,
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
pub struct AttemptBook {
    attempts: HashMap<ClusterAttemptId, AttemptState>,
    by_job: HashMap<String, ClusterAttemptId>,
}

impl AttemptBook {
    pub fn open(&mut self, attempt: ClusterAttemptId, state: AttemptState) {
        for m in &state.members {
            self.by_job.insert(m.job_id.clone(), attempt);
        }
        self.attempts.insert(attempt, state);
    }

    pub fn attempt_of(&self, job_id: &str) -> Option<ClusterAttemptId> {
        self.by_job.get(job_id).copied()
    }

    pub fn accept(&mut self, job_id: &str) -> Acceptance {
        let Some(attempt) = self.attempt_of(job_id) else {
            return Acceptance::NotMember;
        };
        let Some(state) = self.attempts.get_mut(&attempt) else {
            return Acceptance::NotMember;
        };
        if let Some(m) = state.members.iter_mut().find(|m| m.job_id == job_id) {
            m.accepted = true;
        }
        if state.started || !state.members.iter().all(|m| m.accepted) {
            return Acceptance::Waiting;
        }

        Acceptance::AllAccepted {
            cluster: state.cluster,
            attempt,
            workers: state.members.iter().map(|m| m.worker.clone()).collect(),
            roster: state.roster.clone(),
        }
    }

    pub fn mark_started(&mut self, attempt: ClusterAttemptId) {
        if let Some(state) = self.attempts.get_mut(&attempt) {
            state.started = true;
        }
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
            .filter(|(_, s)| !s.started && s.deadline <= now)
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

    pub fn get(&self, attempt: ClusterAttemptId) -> Option<&AttemptState> {
        self.attempts.get(&attempt)
    }

    pub fn get_mut(&mut self, attempt: ClusterAttemptId) -> Option<&mut AttemptState> {
        self.attempts.get_mut(&attempt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::ClusterJobId;

    fn member(job: &str, worker: &str, role: &str, index: u32) -> AttemptMember {
        AttemptMember {
            job_id: job.into(),
            worker: worker.into(),
            role: role.into(),
            index,
            primary: false,
            accepted: false,
            outcome: None,
        }
    }

    fn book(deadline: Instant) -> (AttemptBook, ClusterAttemptId) {
        let cluster = ClusterJobId::now_v7();
        let attempt = ClusterAttemptId::now_v7();
        let mut book = AttemptBook::default();
        book.open(
            attempt,
            AttemptState {
                cluster,
                parked: PendingCluster {
                    id: cluster,
                    same_zone: false,
                    queued_at: gradient_types::now(),
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
            },
        );

        (book, attempt)
    }

    #[test]
    fn the_last_acceptance_starts_the_attempt() {
        let (mut book, attempt) = book(Instant::now() + Duration::from_secs(30));

        assert!(matches!(book.accept("build:a"), Acceptance::Waiting));
        let Acceptance::AllAccepted {
            attempt: started,
            workers,
            ..
        } = book.accept("build:b")
        else {
            panic!("all accepted");
        };
        assert_eq!(started, attempt);
        assert_eq!(workers, vec!["w1".to_owned(), "w2".to_owned()]);
        assert!(matches!(book.accept("eval:x"), Acceptance::NotMember));
    }

    #[test]
    fn only_an_unstarted_attempt_past_its_deadline_is_overdue() {
        let now = Instant::now();
        let (mut book, attempt) = book(now);

        assert_eq!(book.overdue(now + Duration::from_secs(1)), vec![attempt]);
        book.mark_started(attempt);
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

        book.mark_started(attempt);
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
}
