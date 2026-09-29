/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet, VecDeque};

pub type SessionId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ObjectKey {
    Nar(String),
    EvalCache(String),
    Rest(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub session: SessionId,
    pub id: u64,
    pub object: ObjectKey,
    pub size: u64,
    /// A small upload: its session serves it before larger queued ones, and a
    /// session with one waiting is served before the others.
    pub priority: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Grant(Request),
    Skip(Request),
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub concurrency: usize,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Committed,
    Failed,
}

/// Server-wide upload budget: round-robin across sessions, FIFO within one,
/// priority requests ahead of the rest, and no request ever bypasses a blocked
/// head.
pub struct AdmissionCore {
    limits: Limits,
    ring: VecDeque<SessionId>,
    queues: HashMap<SessionId, VecDeque<Request>>,
    granted: HashMap<(SessionId, u64), Request>,
    leaders: HashSet<ObjectKey>,
    followers: HashMap<ObjectKey, Vec<Request>>,
    bytes_in_flight: u64,
}

impl AdmissionCore {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            ring: VecDeque::new(),
            queues: HashMap::new(),
            granted: HashMap::new(),
            leaders: HashSet::new(),
            followers: HashMap::new(),
            bytes_in_flight: 0,
        }
    }

    pub fn in_flight(&self) -> usize {
        self.granted.len()
    }

    pub fn bytes_in_flight(&self) -> u64 {
        self.bytes_in_flight
    }

    pub fn queued(&self, session: SessionId) -> usize {
        self.queues.get(&session).map_or(0, VecDeque::len)
    }

    pub fn queued_by_session(&self) -> Vec<(SessionId, usize)> {
        self.queues.iter().map(|(s, q)| (*s, q.len())).collect()
    }

    pub fn enqueue(&mut self, request: Request) -> Vec<Decision> {
        self.push_back(request);
        self.pump()
    }

    pub fn release(&mut self, session: SessionId, id: u64, outcome: Outcome) -> Vec<Decision> {
        let mut decisions = self.finish(session, id, outcome);
        decisions.extend(self.pump());
        decisions
    }

    pub fn cancel(&mut self, session: SessionId, id: u64) -> Vec<Decision> {
        if self.granted.contains_key(&(session, id)) {
            return self.release(session, id, Outcome::Failed);
        }
        if let Some(queue) = self.queues.get_mut(&session) {
            queue.retain(|r| r.id != id);
        }
        for followers in self.followers.values_mut() {
            followers.retain(|r| !(r.session == session && r.id == id));
        }
        self.drop_empty(session);
        Vec::new()
    }

    pub fn remove_session(&mut self, session: SessionId) -> Vec<Decision> {
        self.queues.remove(&session);
        self.ring.retain(|s| *s != session);
        for followers in self.followers.values_mut() {
            followers.retain(|r| r.session != session);
        }
        let held: Vec<u64> = self
            .granted
            .keys()
            .filter(|(s, _)| *s == session)
            .map(|(_, id)| *id)
            .collect();
        let mut decisions = Vec::new();
        for id in held {
            decisions.extend(self.finish(session, id, Outcome::Failed));
        }
        decisions.extend(self.pump());
        decisions
    }

    fn finish(&mut self, session: SessionId, id: u64, outcome: Outcome) -> Vec<Decision> {
        let Some(request) = self.granted.remove(&(session, id)) else {
            return Vec::new();
        };
        self.bytes_in_flight -= request.size;
        self.leaders.remove(&request.object);
        let followers = self.followers.remove(&request.object).unwrap_or_default();
        match outcome {
            Outcome::Committed => followers.into_iter().map(Decision::Skip).collect(),
            Outcome::Failed => {
                self.requeue_in_order(followers);
                Vec::new()
            }
        }
    }

    fn pump(&mut self) -> Vec<Decision> {
        let mut decisions = Vec::new();
        while let Some(head) = self.head() {
            if self.leaders.contains(&head.object) {
                self.pop_head();
                self.followers
                    .entry(head.object.clone())
                    .or_default()
                    .push(head);
                continue;
            }
            if !self.fits(head.size) {
                break;
            }
            self.pop_head();
            if self.ring.front() == Some(&head.session) {
                self.ring.rotate_left(1);
            }
            self.bytes_in_flight += head.size;
            self.leaders.insert(head.object.clone());
            self.granted.insert((head.session, head.id), head.clone());
            decisions.push(Decision::Grant(head));
        }
        decisions
    }

    fn head(&mut self) -> Option<Request> {
        if let Some(at) = self.ring.iter().position(|s| {
            self.queues
                .get(s)
                .and_then(VecDeque::front)
                .is_some_and(|r| r.priority)
        }) {
            self.ring.rotate_left(at);
        }
        let session = self.ring.front()?;
        self.queues.get(session)?.front().cloned()
    }

    fn pop_head(&mut self) {
        let Some(&session) = self.ring.front() else {
            return;
        };
        if let Some(queue) = self.queues.get_mut(&session) {
            queue.pop_front();
        }
        self.drop_empty(session);
    }

    fn fits(&self, size: u64) -> bool {
        self.granted.is_empty()
            || (self.granted.len() < self.limits.concurrency
                && self.bytes_in_flight + size <= self.limits.bytes)
    }

    fn push_back(&mut self, request: Request) {
        let session = request.session;
        let queue = self.queues.entry(session).or_default();
        if queue.is_empty() {
            self.ring.push_back(session);
        }
        let at = if request.priority {
            queue
                .iter()
                .position(|r| !r.priority)
                .unwrap_or(queue.len())
        } else {
            queue.len()
        };
        queue.insert(at, request);
    }

    fn requeue_in_order(&mut self, followers: Vec<Request>) {
        for follower in followers.iter().rev() {
            self.queues
                .entry(follower.session)
                .or_default()
                .push_front(follower.clone());
        }
        for follower in &followers {
            if !self.ring.contains(&follower.session) {
                self.ring.push_back(follower.session);
            }
        }
    }

    fn drop_empty(&mut self, session: SessionId) {
        if self.queues.get(&session).is_some_and(VecDeque::is_empty) {
            self.queues.remove(&session);
            self.ring.retain(|s| *s != session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn req(session: SessionId, id: u64, hash: &str, size: u64) -> Request {
        Request {
            session,
            id,
            object: ObjectKey::Nar(hash.into()),
            size,
            priority: false,
        }
    }

    fn small_req(session: SessionId, id: u64, hash: &str) -> Request {
        Request {
            priority: true,
            ..req(session, id, hash, 1)
        }
    }

    #[test]
    fn a_small_upload_is_granted_before_the_larger_ones_queued_ahead_of_it() {
        let mut core = core(1, GIB);
        assert_eq!(granted(&core.enqueue(req(1, 1, "a", 1))), vec![(1, 1)]);
        core.enqueue(req(1, 2, "b", 1));
        core.enqueue(req(2, 3, "c", 1));
        core.enqueue(small_req(2, 4, "d"));
        core.enqueue(small_req(2, 5, "e"));

        let mut served = Vec::new();
        let mut last = (1, 1);
        for _ in 0..4 {
            let next = granted(&core.release(last.0, last.1, Outcome::Committed));
            assert_eq!(next.len(), 1, "one slot: {next:?}");
            last = next[0];
            served.push(last);
        }
        assert_eq!(served, vec![(2, 4), (2, 5), (1, 2), (2, 3)]);
    }

    fn granted(decisions: &[Decision]) -> Vec<(SessionId, u64)> {
        decisions
            .iter()
            .filter_map(|d| match d {
                Decision::Grant(r) => Some((r.session, r.id)),
                Decision::Skip(_) => None,
            })
            .collect()
    }

    fn core(concurrency: usize, bytes: u64) -> AdmissionCore {
        AdmissionCore::new(Limits { concurrency, bytes })
    }

    #[test]
    fn grants_up_to_the_concurrency_limit_and_queues_the_rest() {
        let mut c = core(2, 10 * GIB);
        assert_eq!(granted(&c.enqueue(req(1, 1, "a", 1))), vec![(1, 1)]);
        assert_eq!(granted(&c.enqueue(req(1, 2, "b", 1))), vec![(1, 2)]);
        assert!(c.enqueue(req(1, 3, "c", 1)).is_empty());
        assert_eq!(c.queued(1), 1);
        assert_eq!(granted(&c.release(1, 1, Outcome::Committed)), vec![(1, 3)]);
    }

    #[test]
    fn freed_permits_rotate_across_sessions() {
        let mut c = core(1, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        c.enqueue(req(1, 2, "b", 1));
        c.enqueue(req(1, 3, "c", 1));
        c.enqueue(req(2, 1, "d", 1));
        assert_eq!(granted(&c.release(1, 1, Outcome::Committed)), vec![(1, 2)]);
        assert_eq!(granted(&c.release(1, 2, Outcome::Committed)), vec![(2, 1)]);
        assert_eq!(granted(&c.release(2, 1, Outcome::Committed)), vec![(1, 3)]);
    }

    #[test]
    fn a_drained_session_keeps_the_ring_order_of_the_rest() {
        let mut c = core(1, 10 * GIB);
        c.enqueue(req(9, 1, "z", 1));
        c.enqueue(req(1, 1, "a", 1));
        c.enqueue(req(2, 1, "b", 1));
        c.enqueue(req(3, 1, "c", 1));
        assert_eq!(granted(&c.release(9, 1, Outcome::Committed)), vec![(1, 1)]);
        assert_eq!(granted(&c.release(1, 1, Outcome::Committed)), vec![(2, 1)]);
    }

    #[test]
    fn the_byte_budget_holds_back_a_request_that_does_not_fit() {
        let mut c = core(16, 10 * GIB);
        assert_eq!(granted(&c.enqueue(req(1, 1, "a", 6 * GIB))), vec![(1, 1)]);
        assert!(c.enqueue(req(2, 1, "b", 6 * GIB)).is_empty());
        assert_eq!(c.bytes_in_flight(), 6 * GIB);
        assert_eq!(granted(&c.release(1, 1, Outcome::Committed)), vec![(2, 1)]);
    }

    #[test]
    fn nothing_bypasses_a_blocked_head() {
        let mut c = core(16, 10 * GIB);
        c.enqueue(req(1, 1, "a", 6 * GIB));
        assert!(c.enqueue(req(2, 1, "b", 6 * GIB)).is_empty());
        assert!(
            c.enqueue(req(3, 1, "c", 1)).is_empty(),
            "a small request must wait behind the blocked head"
        );
        assert_eq!(
            granted(&c.release(1, 1, Outcome::Committed)),
            vec![(2, 1), (3, 1)]
        );
    }

    #[test]
    fn an_oversized_request_runs_alone_once_idle() {
        let mut c = core(16, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        assert!(c.enqueue(req(2, 1, "big", 20 * GIB)).is_empty());
        assert_eq!(granted(&c.release(1, 1, Outcome::Committed)), vec![(2, 1)]);
        assert!(
            c.enqueue(req(3, 1, "c", 1)).is_empty(),
            "the oversized upload runs alone"
        );
        assert_eq!(granted(&c.release(2, 1, Outcome::Committed)), vec![(3, 1)]);
    }

    #[test]
    fn a_duplicate_object_follows_its_leader_and_skips_on_commit() {
        let mut c = core(16, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        assert!(c.enqueue(req(2, 7, "a", 1)).is_empty());
        assert_eq!(c.in_flight(), 1, "a follower holds no permit");
        assert_eq!(
            c.release(1, 1, Outcome::Committed),
            vec![Decision::Skip(req(2, 7, "a", 1))]
        );
    }

    #[test]
    fn a_follower_does_not_block_its_sessions_other_requests() {
        let mut c = core(16, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        c.enqueue(req(2, 1, "a", 1));
        assert_eq!(granted(&c.enqueue(req(2, 2, "b", 1))), vec![(2, 2)]);
    }

    #[test]
    fn a_failed_leader_promotes_its_follower() {
        let mut c = core(16, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        c.enqueue(req(2, 1, "a", 1));
        c.enqueue(req(3, 1, "a", 1));
        assert_eq!(granted(&c.release(1, 1, Outcome::Failed)), vec![(2, 1)]);
        assert_eq!(
            c.release(2, 1, Outcome::Committed),
            vec![Decision::Skip(req(3, 1, "a", 1))]
        );
    }

    #[test]
    fn cancelling_a_queued_request_consumes_no_permit() {
        let mut c = core(1, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        c.enqueue(req(1, 2, "b", 1));
        assert!(c.cancel(1, 2).is_empty());
        assert!(c.release(1, 1, Outcome::Committed).is_empty());
        assert_eq!(c.in_flight(), 0);
    }

    #[test]
    fn cancelling_a_granted_request_frees_its_permit() {
        let mut c = core(1, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        c.enqueue(req(2, 1, "b", 1));
        assert_eq!(granted(&c.cancel(1, 1)), vec![(2, 1)]);
    }

    #[test]
    fn removing_a_session_frees_everything_it_held() {
        let mut c = core(2, 10 * GIB);
        c.enqueue(req(1, 1, "a", 1));
        c.enqueue(req(1, 2, "b", 1));
        c.enqueue(req(1, 3, "c", 1));
        c.enqueue(req(2, 1, "d", 1));
        c.enqueue(req(2, 2, "a", 1));
        let decisions = c.remove_session(1);
        assert_eq!(granted(&decisions), vec![(2, 1), (2, 2)]);
        assert_eq!(c.queued(1), 0);
        assert_eq!(c.in_flight(), 2);
    }
}
