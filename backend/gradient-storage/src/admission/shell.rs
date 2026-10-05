/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gradient_util::sync::Mutex;
use tokio::sync::mpsc;

use super::core::{AdmissionCore, Decision, Limits, ObjectKey, Outcome, Request, SessionId};

pub enum Admitted {
    Granted {
        id: u64,
        object: ObjectKey,
        permit: UploadPermit,
    },
    Skip {
        id: u64,
    },
}

#[derive(Debug, Default)]
pub struct AdmissionStats {
    pub in_flight: usize,
    pub bytes_in_flight: u64,
    pub queued: Vec<(String, usize)>,
    pub granted_total: u64,
    pub wait_seconds_total: f64,
}

pub struct UploadAdmission {
    core: Mutex<AdmissionCore>,
    sessions: Mutex<HashMap<SessionId, (String, mpsc::UnboundedSender<Admitted>)>>,
    enqueued_at: Mutex<HashMap<(SessionId, u64), Instant>>,
    next_session: AtomicU64,
    granted_total: AtomicU64,
    wait_micros_total: AtomicU64,
}

impl std::fmt::Debug for UploadAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadAdmission")
            .field("in_flight", &self.in_flight())
            .field("bytes_in_flight", &self.bytes_in_flight())
            .finish()
    }
}

impl UploadAdmission {
    pub fn new(limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            core: Mutex::new(AdmissionCore::new(limits)),
            sessions: Mutex::new(HashMap::new()),
            enqueued_at: Mutex::new(HashMap::new()),
            next_session: AtomicU64::new(1),
            granted_total: AtomicU64::new(0),
            wait_micros_total: AtomicU64::new(0),
        })
    }

    pub fn open_session(
        self: &Arc<Self>,
        label: &str,
    ) -> (AdmissionSession, mpsc::UnboundedReceiver<Admitted>) {
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.sessions.lock().insert(id, (label.to_owned(), tx));
        (
            AdmissionSession {
                admission: Arc::clone(self),
                id,
            },
            rx,
        )
    }

    pub fn in_flight(&self) -> usize {
        self.core.lock().in_flight()
    }

    pub fn bytes_in_flight(&self) -> u64 {
        self.core.lock().bytes_in_flight()
    }

    pub fn stats(&self) -> AdmissionStats {
        let (in_flight, bytes_in_flight, by_session) = {
            let core = self.core.lock();
            (
                core.in_flight(),
                core.bytes_in_flight(),
                core.queued_by_session(),
            )
        };
        let sessions = self.sessions.lock();
        let mut queued: HashMap<String, usize> = HashMap::new();
        for (session, n) in by_session {
            if let Some((label, _)) = sessions.get(&session) {
                *queued.entry(label.clone()).or_default() += n;
            }
        }
        AdmissionStats {
            in_flight,
            bytes_in_flight,
            queued: queued.into_iter().collect(),
            granted_total: self.granted_total.load(Ordering::Relaxed),
            wait_seconds_total: self.wait_micros_total.load(Ordering::Relaxed) as f64 / 1e6,
        }
    }

    pub async fn admit(
        self: &Arc<Self>,
        label: &str,
        object: ObjectKey,
        size: u64,
        wait: Duration,
    ) -> Option<Admission> {
        let (session, mut admitted) = self.open_session(label);
        session.request(0, object, size, false);
        match tokio::time::timeout(wait, admitted.recv()).await {
            Ok(Some(Admitted::Granted { permit, .. })) => Some(Admission::Granted(HeldPermit {
                permit,
                _session: session,
            })),
            Ok(Some(Admitted::Skip { .. })) => Some(Admission::AlreadyCommitted),
            _ => None,
        }
    }

    fn record_grant(&self, session: SessionId, id: u64) {
        if let Some(at) = self.enqueued_at.lock().remove(&(session, id)) {
            let waited = at.elapsed().as_micros().min(u64::MAX as u128) as u64;
            self.wait_micros_total.fetch_add(waited, Ordering::Relaxed);
        }
        self.granted_total.fetch_add(1, Ordering::Relaxed);
    }

    fn apply(self: &Arc<Self>, change: impl FnOnce(&mut AdmissionCore) -> Vec<Decision>) {
        let decisions = change(&mut self.core.lock());
        for decision in decisions {
            self.deliver(decision);
        }
    }

    fn deliver(self: &Arc<Self>, decision: Decision) {
        let (session, admitted) = match decision {
            Decision::Grant(Request {
                session,
                id,
                object,
                ..
            }) => (
                session,
                Admitted::Granted {
                    id,
                    object,
                    permit: UploadPermit {
                        admission: Arc::clone(self),
                        session,
                        id,
                        settled: false,
                    },
                },
            ),
            Decision::Skip(Request { session, id, .. }) => (session, Admitted::Skip { id }),
        };
        match &admitted {
            Admitted::Granted { id, .. } => self.record_grant(session, *id),
            Admitted::Skip { id } => {
                self.enqueued_at.lock().remove(&(session, *id));
            }
        }
        let tx = self.sessions.lock().get(&session).map(|(_, tx)| tx.clone());
        if let Some(tx) = tx {
            let _ = tx.send(admitted);
        }
    }
}

pub enum Admission {
    Granted(HeldPermit),
    AlreadyCommitted,
}

pub struct HeldPermit {
    permit: UploadPermit,
    _session: AdmissionSession,
}

impl HeldPermit {
    pub fn committed(self) {
        self.permit.committed();
    }
}

pub struct AdmissionSession {
    admission: Arc<UploadAdmission>,
    id: SessionId,
}

impl AdmissionSession {
    pub fn request(&self, id: u64, object: ObjectKey, size: u64, priority: bool) {
        let session = self.id;
        self.admission
            .enqueued_at
            .lock()
            .insert((session, id), Instant::now());
        self.admission.apply(|core| {
            core.enqueue(Request {
                session,
                id,
                object,
                size,
                priority,
            })
        });
    }

    pub fn cancel(&self, id: u64) {
        let session = self.id;
        self.admission.enqueued_at.lock().remove(&(session, id));
        self.admission.apply(|core| core.cancel(session, id));
    }
}

impl Drop for AdmissionSession {
    fn drop(&mut self) {
        self.admission.sessions.lock().remove(&self.id);
        let session = self.id;
        self.admission
            .enqueued_at
            .lock()
            .retain(|(s, _), _| *s != session);
        self.admission.apply(|core| core.remove_session(session));
    }
}

/// A permit dropped without `committed` is counted as a failed upload. Every exit path is thereby
/// returning the budget.
pub struct UploadPermit {
    admission: Arc<UploadAdmission>,
    session: SessionId,
    id: u64,
    settled: bool,
}

impl UploadPermit {
    pub fn committed(mut self) {
        self.settle(Outcome::Committed);
    }

    pub fn release_budget(&mut self) {
        let (session, id) = (self.session, self.id);
        self.admission
            .apply(|core| core.release_budget(session, id));
    }

    fn settle(&mut self, outcome: Outcome) {
        self.settled = true;
        let (session, id) = (self.session, self.id);
        self.admission
            .apply(|core| core.release(session, id, outcome));
    }
}

impl Drop for UploadPermit {
    fn drop(&mut self) {
        if !self.settled {
            self.settle(Outcome::Failed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn nar(hash: &str) -> ObjectKey {
        ObjectKey::Nar(hash.into())
    }

    fn admission(concurrency: usize) -> Arc<UploadAdmission> {
        UploadAdmission::new(Limits {
            concurrency,
            bytes: u64::MAX,
        })
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<Admitted>) -> Admitted {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("no admission arrived")
            .expect("session channel closed")
    }

    fn idle(rx: &mut mpsc::UnboundedReceiver<Admitted>) -> bool {
        rx.try_recv().is_err()
    }

    #[tokio::test]
    async fn a_dropped_permit_grants_the_next_request() {
        let admission = admission(1);
        let (session, mut rx) = admission.open_session("test");
        session.request(1, nar("a"), 1, false);
        session.request(2, nar("b"), 1, false);
        let Admitted::Granted { id: 1, permit, .. } = next(&mut rx).await else {
            panic!("request 1 is granted first");
        };
        assert!(idle(&mut rx), "request 2 waits for the permit");
        drop(permit);
        assert!(matches!(
            next(&mut rx).await,
            Admitted::Granted { id: 2, .. }
        ));
    }

    #[tokio::test]
    async fn a_committed_permit_skips_the_same_object_in_another_session() {
        let admission = admission(4);
        let (first, mut first_rx) = admission.open_session("test");
        let (second, mut second_rx) = admission.open_session("test");
        first.request(1, nar("a"), 1, false);
        second.request(9, nar("a"), 1, false);
        let Admitted::Granted { permit, .. } = next(&mut first_rx).await else {
            panic!("the first request leads");
        };
        assert!(idle(&mut second_rx));
        permit.committed();
        assert!(matches!(
            next(&mut second_rx).await,
            Admitted::Skip { id: 9 }
        ));
    }

    #[tokio::test]
    async fn a_dropped_session_returns_its_permits_and_grants_the_next_worker() {
        let admission = admission(1);
        let (first, mut first_rx) = admission.open_session("test");
        let (second, mut second_rx) = admission.open_session("test");
        first.request(1, nar("a"), 1, false);
        second.request(1, nar("b"), 1, false);
        let Admitted::Granted { permit, .. } = next(&mut first_rx).await else {
            panic!("the first worker is granted");
        };
        drop(first);
        let Admitted::Granted {
            id: 1,
            permit: held,
            ..
        } = next(&mut second_rx).await
        else {
            panic!("the second worker is granted");
        };
        drop(permit);
        assert_eq!(
            admission.in_flight(),
            1,
            "a stale permit of a gone session frees nothing twice"
        );
        drop(held);
    }

    #[tokio::test]
    async fn cancelling_a_queued_request_never_grants_it() {
        let admission = admission(1);
        let (session, mut rx) = admission.open_session("test");
        session.request(1, nar("a"), 1, false);
        session.request(2, nar("b"), 1, false);
        let Admitted::Granted { permit, .. } = next(&mut rx).await else {
            panic!("request 1 is granted");
        };
        session.cancel(2);
        drop(permit);
        assert!(idle(&mut rx));
        assert_eq!(admission.in_flight(), 0);
    }

    #[tokio::test]
    async fn stats_report_queue_depth_per_label_and_count_grants() {
        let admission = admission(1);
        let (a, _a_rx) = admission.open_session("worker-a");
        let (b, _b_rx) = admission.open_session("worker-b");
        a.request(1, nar("x"), 1, false);
        b.request(1, nar("y"), 1, false);
        b.request(2, nar("z"), 1, false);
        let stats = admission.stats();
        assert_eq!(stats.in_flight, 1);
        assert_eq!(stats.granted_total, 1);
        assert!(stats.queued.contains(&("worker-b".to_owned(), 2)));
    }
}
