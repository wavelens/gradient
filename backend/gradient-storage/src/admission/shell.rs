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
use gradient_wire::messages::is_small_upload;
use tokio::sync::mpsc;

use super::core::{AdmissionCore, Limits, ObjectKey, Request, SessionId};

pub struct Admitted {
    pub id: u64,
    pub object: ObjectKey,
    pub permit: UploadPermit,
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
    ) -> Option<HeldPermit> {
        let (session, mut admitted) = self.open_session(label);
        session.request(0, object, size, is_small_upload(size));
        match tokio::time::timeout(wait, admitted.recv()).await {
            Ok(Some(Admitted { permit, .. })) => Some(HeldPermit {
                _permit: permit,
                _session: session,
            }),
            _ => None,
        }
    }

    fn record_grant(&self, session: SessionId, id: u64) {
        if let Some(at) = self.enqueued_at.lock().remove(&(session, id)) {
            let waited = u64::try_from(at.elapsed().as_micros()).unwrap_or(u64::MAX);
            self.wait_micros_total.fetch_add(waited, Ordering::Relaxed);
        }
        self.granted_total.fetch_add(1, Ordering::Relaxed);
    }

    fn apply(self: &Arc<Self>, change: impl FnOnce(&mut AdmissionCore) -> Vec<Request>) {
        let grants = change(&mut self.core.lock());
        for grant in grants {
            self.deliver(grant);
        }
    }

    fn deliver(self: &Arc<Self>, grant: Request) {
        let Request {
            session,
            id,
            object,
            ..
        } = grant;
        self.record_grant(session, id);
        let admitted = Admitted {
            id,
            object,
            permit: UploadPermit {
                admission: Arc::clone(self),
                session,
                id,
            },
        };
        let tx = self.sessions.lock().get(&session).map(|(_, tx)| tx.clone());
        if let Some(tx) = tx {
            let _ = tx.send(admitted);
        }
    }
}

pub struct HeldPermit {
    _permit: UploadPermit,
    _session: AdmissionSession,
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

/// Drops of `UploadPermit` end the upload on every exit path. Those drops return the budget and
/// grant the next upload of the same object.
pub struct UploadPermit {
    admission: Arc<UploadAdmission>,
    session: SessionId,
    id: u64,
}

impl UploadPermit {
    pub fn release_budget(&mut self) {
        let (session, id) = (self.session, self.id);
        self.admission
            .apply(|core| core.release_budget(session, id));
    }
}

impl Drop for UploadPermit {
    fn drop(&mut self) {
        let (session, id) = (self.session, self.id);
        self.admission.apply(|core| core.release(session, id));
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
        let Admitted { id: 1, permit, .. } = next(&mut rx).await else {
            panic!("request 1 is granted first");
        };
        assert!(idle(&mut rx), "request 2 waits for the permit");
        drop(permit);
        assert!(matches!(next(&mut rx).await, Admitted { id: 2, .. }));
    }

    #[tokio::test]
    async fn upload_permit_is_granted_to_waiting_session_when_upload_ends() {
        let admission = admission(4);
        let (first, mut first_rx) = admission.open_session("test");
        let (second, mut second_rx) = admission.open_session("test");
        first.request(1, nar("a"), 1, false);
        second.request(9, nar("a"), 1, false);
        let Admitted { permit, .. } = next(&mut first_rx).await;
        assert!(idle(&mut second_rx));
        drop(permit);
        assert!(matches!(next(&mut second_rx).await, Admitted { id: 9, .. }));
    }

    #[tokio::test]
    async fn a_dropped_session_returns_its_permits_and_grants_the_next_worker() {
        let admission = admission(1);
        let (first, mut first_rx) = admission.open_session("test");
        let (second, mut second_rx) = admission.open_session("test");
        first.request(1, nar("a"), 1, false);
        second.request(1, nar("b"), 1, false);
        let Admitted { permit, .. } = next(&mut first_rx).await;
        drop(first);
        let Admitted {
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
    async fn small_upload_is_admitted_when_all_regular_upload_slots_are_taken() {
        let admission = admission(1);
        let large = gradient_wire::messages::SMALL_UPLOAD_BYTES + 1;
        let _held = admission
            .admit("test", nar("a"), large, Duration::from_secs(1))
            .await;
        assert!(
            admission
                .admit("test", nar("b"), large, Duration::from_millis(50))
                .await
                .is_none()
        );
        assert!(
            admission
                .admit("test", nar("c"), 1, Duration::from_millis(50))
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn cancelling_a_queued_request_never_grants_it() {
        let admission = admission(1);
        let (session, mut rx) = admission.open_session("test");
        session.request(1, nar("a"), 1, false);
        session.request(2, nar("b"), 1, false);
        let Admitted { permit, .. } = next(&mut rx).await;
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
