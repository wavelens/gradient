/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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

pub struct UploadAdmission {
    core: Mutex<AdmissionCore>,
    sessions: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Admitted>>>,
    next_session: AtomicU64,
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
            next_session: AtomicU64::new(1),
        })
    }

    pub fn open_session(self: &Arc<Self>) -> (AdmissionSession, mpsc::UnboundedReceiver<Admitted>) {
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.sessions.lock().insert(id, tx);
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
        let tx = self.sessions.lock().get(&session).cloned();
        if let Some(tx) = tx {
            let _ = tx.send(admitted);
        }
    }
}

pub struct AdmissionSession {
    admission: Arc<UploadAdmission>,
    id: SessionId,
}

impl AdmissionSession {
    pub fn request(&self, id: u64, object: ObjectKey, size: u64) {
        let session = self.id;
        self.admission.apply(|core| {
            core.enqueue(Request {
                session,
                id,
                object,
                size,
            })
        });
    }

    pub fn cancel(&self, id: u64) {
        let session = self.id;
        self.admission.apply(|core| core.cancel(session, id));
    }
}

impl Drop for AdmissionSession {
    fn drop(&mut self) {
        self.admission.sessions.lock().remove(&self.id);
        let session = self.id;
        self.admission.apply(|core| core.remove_session(session));
    }
}

/// One admitted upload. Dropping it without [`UploadPermit::committed`] counts
/// as a failed upload, so every exit path returns the budget.
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
        let (session, mut rx) = admission.open_session();
        session.request(1, nar("a"), 1);
        session.request(2, nar("b"), 1);
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
        let (first, mut first_rx) = admission.open_session();
        let (second, mut second_rx) = admission.open_session();
        first.request(1, nar("a"), 1);
        second.request(9, nar("a"), 1);
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
        let (first, mut first_rx) = admission.open_session();
        let (second, mut second_rx) = admission.open_session();
        first.request(1, nar("a"), 1);
        second.request(1, nar("b"), 1);
        let Admitted::Granted { permit, .. } = next(&mut first_rx).await else {
            panic!("the first worker is granted");
        };
        drop(first);
        assert!(matches!(
            next(&mut second_rx).await,
            Admitted::Granted { id: 1, .. }
        ));
        drop(permit);
        assert_eq!(
            admission.in_flight(),
            1,
            "a stale permit of a gone session frees nothing twice"
        );
    }

    #[tokio::test]
    async fn cancelling_a_queued_request_never_grants_it() {
        let admission = admission(1);
        let (session, mut rx) = admission.open_session();
        session.request(1, nar("a"), 1);
        session.request(2, nar("b"), 1);
        let Admitted::Granted { permit, .. } = next(&mut rx).await else {
            panic!("request 1 is granted");
        };
        session.cancel(2);
        drop(permit);
        assert!(idle(&mut rx));
        assert_eq!(admission.in_flight(), 0);
    }
}
