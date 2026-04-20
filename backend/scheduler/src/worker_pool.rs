/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-memory registry of connected proto workers.
//!
//! Workers are stored as [`WorkerSlot`] values — either
//! [`WorkerSlot::Active`] or [`WorkerSlot::Draining`].  Capacity checks are
//! only ever performed on `Active` workers, so the compiler prevents the class
//! of bug where a draining worker accidentally receives a new job assignment.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

use gradient_core::types::proto::{GradientCapabilities, JobKind};

use crate::peer_auth::PeerAuth;
use crate::worker_state::{Active, Draining, TypedWorker};

// ── WorkerSlot ────────────────────────────────────────────────────────────────

/// Lifecycle state of a connected worker as seen by the pool.
///
/// Only [`WorkerSlot::Active`] workers can receive new job offers.
pub enum WorkerSlot {
    /// Worker is active — eligible for new job assignments.
    Active(TypedWorker<Active>),
    /// Worker is draining — finishes in-flight jobs but accepts no new ones.
    Draining(TypedWorker<Draining>),
}

impl WorkerSlot {
    /// Read-only access to the shared worker data, regardless of state.
    fn shared(&self) -> &crate::worker_state::WorkerShared {
        match self {
            Self::Active(w) => w,
            Self::Draining(w) => w,
        }
    }

    /// Mutable access to the shared worker data, regardless of state.
    fn shared_mut(&mut self) -> &mut crate::worker_state::WorkerShared {
        match self {
            Self::Active(w) => w,
            Self::Draining(w) => w,
        }
    }

    /// Returns `true` when the worker is draining.
    pub fn is_draining(&self) -> bool {
        matches!(self, Self::Draining(_))
    }
}

// Manual Debug impl because TypedWorker<S> impls Debug
impl std::fmt::Debug for WorkerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active(w) => f.debug_tuple("Active").field(&w.shared).finish(),
            Self::Draining(w) => f.debug_tuple("Draining").field(&w.shared).finish(),
        }
    }
}

// ── WorkerPool ────────────────────────────────────────────────────────────────

/// In-memory registry of all currently connected workers.
#[derive(Debug, Default)]
pub struct WorkerPool {
    workers: HashMap<String, WorkerSlot>,
}

impl WorkerPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.workers.contains_key(id)
    }

    pub fn register(
        &mut self,
        id: String,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<Uuid>,
    ) -> (Arc<Notify>, mpsc::UnboundedReceiver<(String, String)>) {
        let notify = Arc::new(Notify::new());
        let (abort_tx, abort_rx) = mpsc::unbounded_channel();
        let worker = TypedWorker::<Active>::new(
            capabilities,
            authorized_peers,
            Arc::clone(&notify),
            abort_tx,
        );
        self.workers.insert(id, WorkerSlot::Active(worker));
        (notify, abort_rx)
    }

    /// Signal a connected worker that its registrations have changed and it
    /// should re-authenticate.  No-op if the worker is not connected.
    pub fn request_reauth(&self, worker_id: &str) {
        if let Some(slot) = self.workers.get(worker_id) {
            slot.shared().reauth_notify.notify_one();
        }
    }

    /// Send an abort message to a connected worker's handler.
    /// Returns `true` if the message was sent (worker connected), `false` otherwise.
    pub fn send_abort(&self, worker_id: &str, job_id: String, reason: String) -> bool {
        if let Some(slot) = self.workers.get(worker_id) {
            slot.shared().abort_tx.send((job_id, reason)).is_ok()
        } else {
            false
        }
    }

    pub fn update_authorized_peers(&mut self, id: &str, authorized_peers: HashSet<Uuid>) {
        if let Some(slot) = self.workers.get_mut(id) {
            slot.shared_mut().peer_auth = PeerAuth::from_peers(authorized_peers);
        }
    }

    /// Returns the peer-auth mode for a worker, or `None` if not connected.
    pub fn peer_auth_for(&self, id: &str) -> Option<&PeerAuth> {
        self.workers.get(id).map(|slot| &slot.shared().peer_auth)
    }

    /// Returns `(architectures, system_features)` for a connected worker.
    /// Returns `None` if the worker is not connected.
    pub fn build_caps_for(&self, id: &str) -> Option<(Vec<String>, Vec<String>)> {
        self.workers.get(id).map(|slot| {
            let s = slot.shared();
            (s.architectures.clone(), s.system_features.clone())
        })
    }

    /// Returns the negotiated `GradientCapabilities` for a connected worker,
    /// or `None` if the worker is not connected.
    pub fn gradient_caps_for(&self, id: &str) -> Option<GradientCapabilities> {
        self.workers
            .get(id)
            .map(|slot| slot.shared().capabilities.clone())
    }

    pub fn update_capabilities(
        &mut self,
        id: &str,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
    ) {
        if let Some(slot) = self.workers.get_mut(id) {
            let s = slot.shared_mut();
            s.architectures = architectures;
            s.system_features = system_features;
            s.max_concurrent_builds = max_concurrent_builds;
        }
    }

    pub fn unregister(&mut self, id: &str) -> Vec<String> {
        self.workers
            .remove(id)
            .map(|slot| slot.shared().assigned_jobs.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Transition a worker to the draining state.
    ///
    /// Draining workers finish their in-flight jobs but are never offered new
    /// ones — [`has_capacity`] returns `false` for draining workers at the type
    /// level.
    pub fn mark_draining(&mut self, id: &str) {
        if let Some(slot) = self.workers.remove(id) {
            let new_slot = match slot {
                WorkerSlot::Active(w) => WorkerSlot::Draining(w.into_draining()),
                already_draining => already_draining,
            };
            self.workers.insert(id.to_owned(), new_slot);
        }
    }

    /// Mark a batch of job IDs as sent to `worker_id` so they are not
    /// re-included in the next delta `JobOffer`.
    pub fn mark_candidates_sent(&mut self, worker_id: &str, job_ids: &[String]) {
        if let Some(slot) = self.workers.get_mut(worker_id) {
            slot.shared_mut()
                .sent_candidates
                .extend(job_ids.iter().cloned());
        }
    }

    /// Remove a single job ID from all workers' sent-candidate sets.
    pub fn remove_sent_candidate(&mut self, job_id: &str) {
        for slot in self.workers.values_mut() {
            slot.shared_mut().sent_candidates.remove(job_id);
        }
    }

    /// Returns the set of job IDs already sent to `worker_id` as candidates.
    pub fn sent_candidates_for(&self, worker_id: &str) -> Option<&HashSet<String>> {
        self.workers
            .get(worker_id)
            .map(|slot| &slot.shared().sent_candidates)
    }

    /// Returns `true` when the worker can accept a new job of the given kind.
    ///
    /// - **Draining workers always return `false`** — this is enforced at the
    ///   type level by only calling `has_build_capacity` on `TypedWorker<Active>`.
    /// - Eval jobs are always accepted by active workers (capacity is enforced
    ///   worker-side).
    /// - Build jobs are gated by `max_concurrent_builds`.
    pub fn has_capacity(&self, worker_id: &str, kind: &JobKind) -> bool {
        match self.workers.get(worker_id) {
            Some(WorkerSlot::Active(w)) => match kind {
                JobKind::Flake | JobKind::Sign => true,
                JobKind::Build => w.has_build_capacity(),
            },
            Some(WorkerSlot::Draining(_)) => false,
            None => false,
        }
    }

    pub fn assign_job(&mut self, worker_id: &str, job_id: &str) {
        if let Some(slot) = self.workers.get_mut(worker_id) {
            slot.shared_mut().assigned_jobs.insert(job_id.to_owned());
        }
    }

    pub fn release_job(&mut self, worker_id: &str, job_id: &str) {
        if let Some(slot) = self.workers.get_mut(worker_id) {
            slot.shared_mut().assigned_jobs.remove(job_id);
        }
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub fn all_workers(&self) -> Vec<WorkerInfo> {
        self.workers
            .iter()
            .map(|(id, slot)| {
                let s = slot.shared();
                WorkerInfo {
                    id: id.clone(),
                    capabilities: s.capabilities.clone(),
                    architectures: s.architectures.clone(),
                    system_features: s.system_features.clone(),
                    max_concurrent_builds: s.max_concurrent_builds,
                    assigned_job_count: s.assigned_jobs.len(),
                    draining: slot.is_draining(),
                }
            })
            .collect()
    }
}

// ── WorkerInfo ────────────────────────────────────────────────────────────────

/// Serialisable snapshot of a connected worker for API responses.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerInfo {
    pub id: String,
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_auth::PeerAuth;

    fn caps() -> GradientCapabilities {
        GradientCapabilities::default()
    }

    #[test]
    fn test_register_and_is_connected() {
        let mut pool = WorkerPool::new();
        assert!(!pool.is_connected("w1"));
        pool.register("w1".into(), caps(), HashSet::new());
        assert!(pool.is_connected("w1"));
        assert_eq!(pool.worker_count(), 1);
    }

    #[test]
    fn test_unregister_returns_assigned_jobs() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());
        pool.assign_job("w1", "j1");
        pool.assign_job("w1", "j2");

        let mut jobs = pool.unregister("w1");
        jobs.sort();
        assert_eq!(jobs, vec!["j1", "j2"]);
        assert!(!pool.is_connected("w1"));
        assert_eq!(pool.worker_count(), 0);
    }

    #[test]
    fn test_unregister_unknown_returns_empty() {
        let mut pool = WorkerPool::new();
        assert!(pool.unregister("w1").is_empty());
    }

    #[test]
    fn test_update_capabilities() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());
        pool.update_capabilities("w1", vec!["x86_64-linux".into()], vec!["kvm".into()], 4);

        let workers = pool.all_workers();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].architectures, vec!["x86_64-linux"]);
        assert_eq!(workers[0].system_features, vec!["kvm"]);
        assert_eq!(workers[0].max_concurrent_builds, 4);
    }

    #[test]
    fn test_mark_draining() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());

        let info = &pool.all_workers()[0];
        assert!(!info.draining);

        pool.mark_draining("w1");
        let info = &pool.all_workers()[0];
        assert!(info.draining);
    }

    #[test]
    fn test_draining_worker_has_no_capacity() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());
        pool.update_capabilities("w1", vec![], vec![], 10);

        // Active worker has capacity.
        assert!(pool.has_capacity("w1", &JobKind::Build));
        assert!(pool.has_capacity("w1", &JobKind::Flake));

        // After draining, capacity is always false.
        pool.mark_draining("w1");
        assert!(!pool.has_capacity("w1", &JobKind::Build));
        assert!(!pool.has_capacity("w1", &JobKind::Flake));
    }

    #[test]
    fn test_authorized_peers_for() {
        let mut pool = WorkerPool::new();
        let peer_a = Uuid::new_v4();
        let peer_b = Uuid::new_v4();

        pool.register("w1".into(), caps(), HashSet::from([peer_a, peer_b]));
        let auth = pool.peer_auth_for("w1").unwrap();
        assert!(auth.contains(&peer_a));
        assert!(auth.contains(&peer_b));
        assert!(matches!(auth, PeerAuth::Restricted(_)));

        assert!(pool.peer_auth_for("w2").is_none());
    }

    #[test]
    fn test_update_authorized_peers() {
        let mut pool = WorkerPool::new();
        let peer_a = Uuid::new_v4();
        let peer_b = Uuid::new_v4();

        pool.register("w1".into(), caps(), HashSet::from([peer_a]));
        assert!(matches!(
            pool.peer_auth_for("w1").unwrap(),
            PeerAuth::Restricted(_)
        ));

        pool.update_authorized_peers("w1", HashSet::from([peer_a, peer_b]));
        let auth = pool.peer_auth_for("w1").unwrap();
        let PeerAuth::Restricted(set) = auth else {
            panic!("expected Restricted");
        };
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_open_mode_on_empty_peers() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());
        assert!(matches!(pool.peer_auth_for("w1").unwrap(), PeerAuth::Open));
    }

    #[test]
    fn build_capacity_strict_at_limit() {
        // Worker at exactly max_concurrent_builds must reject new builds.
        // Guards against `<` → `<=` off-by-one in `has_build_capacity`.
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());
        pool.update_capabilities("w1", vec!["x86_64-linux".into()], vec![], 2);

        assert!(pool.has_capacity("w1", &JobKind::Build), "0/2 has capacity");
        pool.assign_job("w1", "j1");
        assert!(pool.has_capacity("w1", &JobKind::Build), "1/2 has capacity");
        pool.assign_job("w1", "j2");
        assert!(
            !pool.has_capacity("w1", &JobKind::Build),
            "2/2 is at limit — must reject"
        );
        pool.release_job("w1", "j2");
        assert!(pool.has_capacity("w1", &JobKind::Build), "1/2 again");
    }

    #[test]
    fn test_assign_and_release_job() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());

        pool.assign_job("w1", "j1");
        assert_eq!(pool.all_workers()[0].assigned_job_count, 1);

        pool.assign_job("w1", "j2");
        assert_eq!(pool.all_workers()[0].assigned_job_count, 2);

        pool.release_job("w1", "j1");
        assert_eq!(pool.all_workers()[0].assigned_job_count, 1);

        pool.release_job("w1", "j2");
        assert_eq!(pool.all_workers()[0].assigned_job_count, 0);
    }

    #[test]
    fn test_all_workers_info() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new());
        pool.register("w2".into(), caps(), HashSet::new());
        pool.update_capabilities("w1", vec!["x86_64-linux".into()], vec![], 2);
        pool.assign_job("w1", "j1");
        pool.mark_draining("w2");

        let mut workers = pool.all_workers();
        workers.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(workers[0].id, "w1");
        assert_eq!(workers[0].assigned_job_count, 1);
        assert!(!workers[0].draining);

        assert_eq!(workers[1].id, "w2");
        assert_eq!(workers[1].assigned_job_count, 0);
        assert!(workers[1].draining);
    }

    #[test]
    fn test_request_reauth_notifies_connected_worker() {
        let mut pool = WorkerPool::new();
        let (notify, _abort_rx) = pool.register("w1".into(), caps(), HashSet::new());

        pool.request_reauth("w1");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(50), notify.notified())
                .await
                .expect("reauth notify should fire immediately");
        });
    }

    #[test]
    fn test_request_reauth_noop_for_unknown_worker() {
        let pool = WorkerPool::new();
        pool.request_reauth("nonexistent");
    }
}
