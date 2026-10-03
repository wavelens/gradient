/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A held cluster member is occupying its slot without running until `StartCluster`.
//! Signals are reaching it through its route.
#![expect(
    dead_code,
    reason = "ClusterInbox and ClusterChannels::{take, send} are the executor handoff; no executor consumes them yet"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    ClientMessage, ClusterAddress, ClusterMembership, ClusterPeer, Job, JobKind,
};
use gradient_worker_client::connection::ProtoWriter;

use crate::proto::credentials::CredentialStore;
use tokio::sync::mpsc;

pub(super) struct HeldJob {
    pub job_id: String,
    pub assignment_id: String,
    pub job: Job,
    pub kind: JobKind,
    pub credentials: CredentialStore,
}

struct HeldAttempt {
    membership: ClusterMembership,
    job: HeldJob,
    release_at: Instant,
}

#[derive(Default)]
pub(super) struct ClusterHolds {
    held: HashMap<String, HeldAttempt>,
}

impl ClusterHolds {
    pub(super) fn hold(
        &mut self,
        membership: ClusterMembership,
        job: HeldJob,
        now: Instant,
    ) -> bool {
        if self.held.contains_key(&membership.attempt) {
            return false;
        }
        let release_at = now + Duration::from_secs(u64::from(membership.hold_secs));
        self.held.insert(
            membership.attempt.clone(),
            HeldAttempt {
                membership,
                job,
                release_at,
            },
        );

        true
    }

    pub(super) fn held(&self, kind: &JobKind) -> u32 {
        self.held.values().filter(|h| &h.job.kind == kind).count() as u32
    }

    pub(super) fn start(&mut self, attempt: &str) -> Option<(ClusterMembership, HeldJob)> {
        self.held.remove(attempt).map(|h| (h.membership, h.job))
    }

    pub(super) fn drop_attempt(&mut self, attempt: &str) -> Option<HeldJob> {
        self.held.remove(attempt).map(|h| h.job)
    }

    pub(super) fn drop_job(&mut self, job_id: &str) -> Option<HeldJob> {
        let attempt = self
            .held
            .iter()
            .find(|(_, h)| h.job.job_id == job_id)
            .map(|(a, _)| a.clone())?;
        self.drop_attempt(&attempt)
    }

    pub(super) fn expired(&mut self, now: Instant) -> Vec<HeldJob> {
        self.release_where(|h| h.release_at <= now)
    }

    pub(super) fn release_all(&mut self) -> Vec<HeldJob> {
        self.release_where(|_| true)
    }

    fn release_where(&mut self, due: impl Fn(&HeldAttempt) -> bool) -> Vec<HeldJob> {
        let attempts: Vec<String> = self
            .held
            .iter()
            .filter(|(_, h)| due(h))
            .map(|(attempt, _)| attempt.clone())
            .collect();

        attempts
            .iter()
            .filter_map(|attempt| self.drop_attempt(attempt))
            .collect()
    }
}

pub(crate) struct ClusterInbox {
    pub attempt: String,
    pub me: ClusterAddress,
    pub roster: Vec<ClusterPeer>,
    pub signals: mpsc::UnboundedReceiver<(ClusterAddress, Vec<u8>)>,
}

struct Route {
    signals: mpsc::UnboundedSender<(ClusterAddress, Vec<u8>)>,
    inbox: Option<ClusterInbox>,
}

#[derive(Default)]
pub(super) struct ClusterRoutes {
    routes: HashMap<String, Route>,
}

impl ClusterRoutes {
    pub(super) fn open(&mut self, attempt: &str, me: ClusterAddress, roster: Vec<ClusterPeer>) {
        let (signals, rx) = mpsc::unbounded_channel();
        let inbox = ClusterInbox {
            attempt: attempt.to_owned(),
            me,
            roster,
            signals: rx,
        };
        self.routes.insert(
            attempt.to_owned(),
            Route {
                signals,
                inbox: Some(inbox),
            },
        );
    }

    pub(super) fn deliver(&self, attempt: &str, from: ClusterAddress, payload: Vec<u8>) -> bool {
        self.routes
            .get(attempt)
            .is_some_and(|route| route.signals.send((from, payload)).is_ok())
    }

    pub(super) fn take(&mut self, attempt: &str) -> Option<ClusterInbox> {
        self.routes.get_mut(attempt)?.inbox.take()
    }

    pub(super) fn close(&mut self, attempt: &str) {
        self.routes.remove(attempt);
    }
}

#[derive(Clone)]
pub(crate) struct ClusterChannels {
    routes: Arc<Mutex<ClusterRoutes>>,
    writer: ProtoWriter,
}

impl ClusterChannels {
    pub(super) fn new(writer: ProtoWriter) -> Self {
        Self {
            routes: Arc::new(Mutex::new(ClusterRoutes::default())),
            writer,
        }
    }

    pub(super) fn open(&self, attempt: &str, me: ClusterAddress, roster: Vec<ClusterPeer>) {
        self.routes.lock().open(attempt, me, roster);
    }

    pub(super) fn deliver(&self, attempt: &str, from: ClusterAddress, payload: Vec<u8>) -> bool {
        self.routes.lock().deliver(attempt, from, payload)
    }

    pub(crate) fn take(&self, attempt: &str) -> Option<ClusterInbox> {
        self.routes.lock().take(attempt)
    }

    pub(super) fn close(&self, attempt: &str) {
        self.routes.lock().close(attempt);
    }

    pub(crate) async fn send(
        &self,
        attempt: &str,
        to: Option<ClusterAddress>,
        payload: Vec<u8>,
    ) -> Result<()> {
        self.writer
            .send(ClientMessage::ClusterSignal {
                attempt: attempt.to_owned(),
                to,
                payload,
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::messages::{BuildJob, Job};

    fn membership(attempt: &str, hold_secs: u32) -> ClusterMembership {
        ClusterMembership {
            attempt: attempt.to_owned(),
            role: "node".to_owned(),
            index: 0,
            hold_secs,
        }
    }

    fn held(job_id: &str) -> HeldJob {
        HeldJob {
            job_id: job_id.to_owned(),
            assignment_id: format!("dispatch-{job_id}"),
            job: Job::Build(BuildJob {
                builds: Vec::new(),
                requirement: Default::default(),
            }),
            kind: JobKind::Build,
            credentials: CredentialStore::new(),
        }
    }

    #[test]
    fn a_held_member_occupies_a_slot_until_started() {
        let mut holds = ClusterHolds::default();
        let now = Instant::now();

        assert!(holds.hold(membership("a1", 40), held("build:x"), now));

        assert_eq!(holds.held(&JobKind::Build), 1);
        assert_eq!(holds.held(&JobKind::Flake), 0);
        let (m, job) = holds.start("a1").expect("the hold starts");
        assert_eq!((m.role.as_str(), job.job_id.as_str()), ("node", "build:x"));
        assert_eq!(holds.held(&JobKind::Build), 0);
    }

    #[test]
    fn a_second_member_of_one_attempt_is_refused() {
        let mut holds = ClusterHolds::default();
        let now = Instant::now();
        holds.hold(membership("a1", 40), held("build:x"), now);

        assert!(!holds.hold(membership("a1", 40), held("build:y"), now));

        let (_, job) = holds.start("a1").expect("the first hold stays");
        assert_eq!(job.job_id, "build:x");
    }

    #[test]
    fn starting_an_unknown_attempt_runs_nothing() {
        let mut holds = ClusterHolds::default();
        holds.hold(membership("a1", 40), held("build:x"), Instant::now());

        assert!(holds.start("a2").is_none());
        assert_eq!(
            holds.held(&JobKind::Build),
            1,
            "the other hold is untouched"
        );
    }
    fn peer() -> ClusterAddress {
        ClusterAddress {
            role: "node".to_owned(),
            index: 1,
        }
    }

    fn me() -> ClusterAddress {
        ClusterAddress {
            role: "node".to_owned(),
            index: 0,
        }
    }

    #[test]
    fn a_signal_reaches_its_attempt_inbox() {
        let mut routes = ClusterRoutes::default();
        routes.open("a1", me(), Vec::new());

        assert!(routes.deliver("a1", peer(), b"ready".to_vec()));

        let mut inbox = routes.take("a1").expect("the executor takes the inbox");
        assert_eq!(inbox.me, me());
        let (from, payload) = inbox
            .signals
            .try_recv()
            .expect("the signal waits in the inbox");
        assert_eq!((from, payload), (peer(), b"ready".to_vec()));
        assert!(routes.take("a1").is_none(), "the inbox is handed out once");
        assert!(
            routes.deliver("a1", peer(), b"again".to_vec()),
            "signals still route after the handoff"
        );
    }

    #[test]
    fn a_signal_after_close_is_dropped() {
        let mut routes = ClusterRoutes::default();
        routes.open("a1", me(), Vec::new());
        routes.close("a1");

        assert!(!routes.deliver("a1", peer(), b"late".to_vec()));
        assert!(!routes.deliver("a2", peer(), b"unknown".to_vec()));
    }
    #[test]
    fn aborting_a_held_attempt_frees_its_slot() {
        let mut holds = ClusterHolds::default();
        holds.hold(membership("a1", 40), held("build:x"), Instant::now());

        let dropped = holds.drop_attempt("a1").expect("the hold is dropped");

        assert_eq!(dropped.job_id, "build:x");
        assert_eq!(holds.held(&JobKind::Build), 0);
        assert!(
            holds.start("a1").is_none(),
            "a late StartCluster runs nothing"
        );
    }

    #[test]
    fn an_aborted_job_drops_its_hold() {
        let mut holds = ClusterHolds::default();
        holds.hold(membership("a1", 40), held("build:x"), Instant::now());

        assert_eq!(holds.drop_job("build:x").expect("held").job_id, "build:x");
        assert!(holds.drop_job("build:x").is_none());
        assert_eq!(holds.held(&JobKind::Build), 0);
    }

    #[test]
    fn a_hold_is_released_at_its_deadline() {
        let mut holds = ClusterHolds::default();
        let now = Instant::now();
        holds.hold(membership("a1", 0), held("build:x"), now);
        holds.hold(membership("a2", 40), held("build:y"), now);

        let released = holds.expired(now);

        assert_eq!(
            released
                .iter()
                .map(|j| j.job_id.as_str())
                .collect::<Vec<_>>(),
            ["build:x"]
        );
        assert_eq!(holds.held(&JobKind::Build), 1);
    }

    #[test]
    fn a_drain_releases_every_hold() {
        let mut holds = ClusterHolds::default();
        let now = Instant::now();
        holds.hold(membership("a1", 40), held("build:x"), now);
        holds.hold(membership("a2", 40), held("build:y"), now);

        assert_eq!(holds.release_all().len(), 2);
        assert_eq!(holds.held(&JobKind::Build), 0);
    }
}
