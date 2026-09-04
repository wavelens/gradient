/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-memory registry of connected proto workers.
//!
//! Workers are stored as [`WorkerSlot`] values - either
//! [`WorkerSlot::Active`] or [`WorkerSlot::Draining`].  Capacity checks are
//! only ever performed on `Active` workers, so the compiler prevents the class
//! of bug where a draining worker accidentally receives a new job assignment.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use gradient_types::ids::ProjectId;
use gradient_types::proto::{GradientCapabilities, JobKind};

use crate::actor::{SessionPort, SessionSignal};
use crate::peer_auth::PeerAuth;
use crate::worker_state::{Active, Draining, TypedWorker};

// ── WorkerSlot ────────────────────────────────────────────────────────────────

/// Lifecycle state of a connected worker as seen by the pool.
///
/// Only [`WorkerSlot::Active`] workers can receive new job offers.
pub enum WorkerSlot {
    /// Worker is active - eligible for new job assignments.
    Active(TypedWorker<Active>),
    /// Worker is draining - finishes in-flight jobs but accepts no new ones.
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
    /// Owning project per worker, resolved from `worker_registration` at
    /// connect time. Used to attribute worker_sample / worker_connection rows.
    worker_projects: HashMap<String, ProjectId>,
}

impl WorkerPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.workers.contains_key(id)
    }

    /// Register a connection, carrying the prior connection's reported sizing
    /// forward so a reconnect without a fresh `WorkerCapabilities` still builds.
    /// Returns the `last_seen` handle the session stamps on every inbound frame.
    pub fn register(
        &mut self,
        id: String,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<ProjectId>,
        session: Arc<dyn SessionPort>,
    ) -> Arc<AtomicI64> {
        let prior = self.workers.get(&id).map(|slot| {
            let s = slot.shared();
            (
                s.architectures.clone(),
                s.system_features.clone(),
                s.max_concurrent_builds,
                s.cpu_count,
                s.ram_total_mb,
                s.cpu_core_score,
            )
        });
        let worker = TypedWorker::<Active>::new(capabilities, authorized_peers, session);
        let last_seen = Arc::clone(&worker.last_seen);
        self.workers.insert(id.clone(), WorkerSlot::Active(worker));
        if let Some((
            architectures,
            system_features,
            max_concurrent_builds,
            cpu_count,
            ram_total_mb,
            cpu_core_score,
        )) = prior
            && let Some(slot) = self.workers.get_mut(&id)
        {
            let s = slot.shared_mut();
            s.architectures = architectures;
            s.system_features = system_features;
            s.max_concurrent_builds = max_concurrent_builds;
            s.cpu_count = cpu_count;
            s.ram_total_mb = ram_total_mb;
            s.cpu_core_score = cpu_core_score;
        }
        last_seen
    }

    /// Record the owning project for a connected worker.
    pub fn set_worker_project(&mut self, id: &str, project: ProjectId) {
        self.worker_projects.insert(id.to_owned(), project);
    }

    pub fn request_reauth(&self, worker_id: &str) {
        if let Some(slot) = self.workers.get(worker_id) {
            slot.shared().session.signal(SessionSignal::Reauth);
        }
    }

    /// Push an abort to the worker's session; `false` when it is not connected.
    pub fn send_abort(&self, worker_id: &str, job_id: String, reason: String) -> bool {
        match self.workers.get(worker_id) {
            Some(slot) => {
                slot.shared()
                    .session
                    .signal(SessionSignal::Abort { job_id, reason });
                true
            }
            None => false,
        }
    }

    /// Push `signal` to every active (not draining) worker's session.
    pub fn signal_active(&self, signal: SessionSignal) {
        for slot in self.workers.values() {
            if let WorkerSlot::Active(w) = slot {
                w.session.signal(signal.clone());
            }
        }
    }

    pub fn update_authorized_peers(&mut self, id: &str, authorized_peers: HashSet<ProjectId>) {
        if let Some(slot) = self.workers.get_mut(id) {
            slot.shared_mut().peer_auth = PeerAuth::from_peers(authorized_peers);
        }
    }

    /// Returns the peer-auth mode for a worker, or `None` if not connected.
    pub fn peer_auth_for(&self, id: &str) -> Option<&PeerAuth> {
        self.workers.get(id).map(|slot| &slot.shared().peer_auth)
    }

    /// Returns the negotiated `GradientCapabilities` for a connected worker,
    /// or `None` if the worker is not connected.
    pub fn gradient_caps_for(&self, id: &str) -> Option<GradientCapabilities> {
        self.workers
            .get(id)
            .map(|slot| slot.shared().capabilities.clone())
    }

    /// One coherent [`WorkerCaps`] snapshot (capabilities, architectures,
    /// features, live metrics) for a connected worker, or `None` if unknown.
    pub fn worker_caps(&self, id: &str) -> Option<crate::jobs::WorkerCaps> {
        self.workers.get(id).map(|slot| {
            let s = slot.shared();
            crate::jobs::WorkerCaps {
                fetch: s.capabilities.fetch,
                architectures: s.architectures.clone(),
                system_features: s.system_features.clone(),
                capabilities: s.capabilities.clone(),
                metrics: self.metrics_for(id),
            }
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WorkerCapabilities wire fields; refactor tracked in #503"
    )]
    pub fn update_capabilities(
        &mut self,
        id: &str,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
        cpu_count: u32,
        ram_total_mb: u64,
        cpu_core_score: u32,
    ) {
        if let Some(slot) = self.workers.get_mut(id) {
            let s = slot.shared_mut();
            s.architectures = architectures;
            s.system_features = system_features;
            s.max_concurrent_builds = max_concurrent_builds;
            s.cpu_count = cpu_count;
            s.ram_total_mb = ram_total_mb;
            s.cpu_core_score = cpu_core_score;
        }
    }

    /// Apply a live-metrics heartbeat to a connected worker. No-op if unknown.
    pub fn update_metrics(
        &mut self,
        id: &str,
        cpu_usage_pct: f32,
        ram_free_mb: u64,
        disk_speed_mbps: Option<f32>,
        network_speed_mbps: Option<f32>,
    ) {
        if let Some(slot) = self.workers.get_mut(id) {
            let s = slot.shared_mut();
            s.cpu_usage_pct = Some(cpu_usage_pct);
            s.ram_free_mb = Some(ram_free_mb);
            s.disk_speed_mbps = disk_speed_mbps;
            s.network_speed_mbps = network_speed_mbps;
        }
    }

    /// Returns a scoring view of a connected worker's static caps and latest
    /// live metrics, or `None` if the worker is not connected.
    pub fn metrics_for(&self, id: &str) -> Option<gradient_score::WorkerMetricsView> {
        self.workers.get(id).map(|slot| {
            let s = slot.shared();
            gradient_score::WorkerMetricsView {
                cpu_count: s.cpu_count,
                cpu_core_score: s.cpu_core_score,
                ram_total_mb: s.ram_total_mb,
                ram_free_mb: s.ram_free_mb,
                cpu_usage_pct: s.cpu_usage_pct,
                disk_speed_mbps: s.disk_speed_mbps,
                network_speed_mbps: s.network_speed_mbps,
            }
        })
    }

    pub fn unregister(&mut self, id: &str) -> Vec<String> {
        self.worker_projects.remove(id);
        self.workers
            .remove(id)
            .map(|slot| slot.shared().assigned_jobs.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Transition a worker to the draining state.
    ///
    /// Draining workers finish their in-flight jobs but are never offered new
    /// ones - [`has_capacity`] returns `false` for draining workers at the type
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
    /// - **Draining workers always return `false`** - this is enforced at the
    ///   type level by only calling `has_build_capacity` on `TypedWorker<Active>`.
    /// - Eval jobs are always accepted by active workers (capacity is enforced
    ///   worker-side).
    /// - Build jobs are gated by `max_concurrent_builds`.
    pub fn has_capacity(&self, worker_id: &str, kind: &JobKind) -> bool {
        match self.workers.get(worker_id) {
            Some(WorkerSlot::Active(w)) => match kind {
                JobKind::Flake => true,
                JobKind::Build => w.has_build_capacity(),
            },
            Some(WorkerSlot::Draining(_)) => false,
            None => false,
        }
    }

    pub fn has_idle_eval_only_worker(&self) -> bool {
        self.workers.values().any(|slot| match slot {
            WorkerSlot::Active(w) => {
                w.capabilities.eval && !w.capabilities.fetch && w.assigned_jobs.is_empty()
            }
            WorkerSlot::Draining(_) => false,
        })
    }

    pub fn assign_job(&mut self, worker_id: &str, job_id: &str) {
        if let Some(slot) = self.workers.get_mut(worker_id) {
            slot.shared_mut().assigned_jobs.insert(job_id.to_owned());
        }
    }

    /// Remove a finished job from the worker's in-flight set. Returns `true` if
    /// the worker now has no assigned jobs (went idle) - the caller kicks the
    /// dispatch loop only then, since feeding a still-busy worker can wait for
    /// the next tick.
    pub fn release_job(&mut self, worker_id: &str, job_id: &str) -> bool {
        match self.workers.get_mut(worker_id) {
            Some(slot) => {
                let shared = slot.shared_mut();
                shared.assigned_jobs.remove(job_id);
                shared.assigned_jobs.is_empty()
            }
            None => false,
        }
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// `(total_workers, idle_workers)` - connected workers and those with no
    /// assigned jobs, fed into the windowed instance snapshot.
    pub fn worker_counts(&self) -> (u32, u32) {
        let total = self.workers.len() as u32;
        let idle = self
            .workers
            .values()
            .filter(|slot| slot.shared().assigned_jobs.is_empty())
            .count() as u32;
        (total, idle)
    }

    fn info_for(&self, id: &str, slot: &WorkerSlot) -> WorkerInfo {
        let s = slot.shared();
        WorkerInfo {
            id: id.to_owned(),
            capabilities: s.capabilities.clone(),
            architectures: s.architectures.clone(),
            system_features: s.system_features.clone(),
            max_concurrent_builds: s.max_concurrent_builds,
            assigned_job_count: s.assigned_jobs.len(),
            draining: slot.is_draining(),
            authorized_peers: s.peer_auth.as_filter().cloned(),
            project: self.worker_projects.get(id).copied(),
            cpu_usage_pct: s.cpu_usage_pct,
            ram_free_mb: s.ram_free_mb,
            ram_total_mb: s.ram_total_mb,
            disk_speed_mbps: s.disk_speed_mbps,
            network_speed_mbps: s.network_speed_mbps,
        }
    }

    pub fn all_workers(&self) -> Vec<WorkerInfo> {
        self.workers
            .iter()
            .map(|(id, slot)| self.info_for(id, slot))
            .collect()
    }

    /// Peers whose last inbound frame is older than `timeout_ms` relative to
    /// `now_ms` - i.e. silent past the heartbeat deadline, so presumed dead.
    pub fn stale_worker_ids(&self, now_ms: i64, timeout_ms: i64) -> Vec<String> {
        self.workers
            .iter()
            .filter(|(_, slot)| {
                now_ms - slot.shared().last_seen.load(Ordering::Relaxed) > timeout_ms
            })
            .map(|(id, _)| id.clone())
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
    /// Peer (project) UUIDs the worker successfully authenticated for. `None`
    /// means the worker is in open mode (no registrations) and is implicitly
    /// authorized for all peers; this should not happen in normal operation
    /// because workers must register with at least one project.
    pub authorized_peers: Option<HashSet<ProjectId>>,
    /// Internal sampling fields (skipped in API output - surfaced via the
    /// access-controlled Job Board APIs, not the existing workers endpoint).
    #[serde(skip)]
    pub project: Option<ProjectId>,
    #[serde(skip)]
    pub cpu_usage_pct: Option<f32>,
    #[serde(skip)]
    pub ram_free_mb: Option<u64>,
    #[serde(skip)]
    pub ram_total_mb: u64,
    #[serde(skip)]
    pub disk_speed_mbps: Option<f32>,
    #[serde(skip)]
    pub network_speed_mbps: Option<f32>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{SessionPort, SessionSignal};
    use crate::peer_auth::PeerAuth;
    use tokio::sync::mpsc;

    fn caps() -> GradientCapabilities {
        GradientCapabilities::default()
    }

    fn port() -> (Arc<dyn SessionPort>, mpsc::UnboundedReceiver<SessionSignal>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(tx), rx)
    }

    fn caps_ef(eval: bool, fetch: bool) -> GradientCapabilities {
        GradientCapabilities {
            eval,
            fetch,
            ..GradientCapabilities::default()
        }
    }

    #[test]
    fn idle_eval_only_worker_detected() {
        let mut pool = WorkerPool::new();
        pool.register("f1".into(), caps_ef(true, true), HashSet::new(), port().0);
        assert!(
            !pool.has_idle_eval_only_worker(),
            "only a fetch worker present"
        );

        pool.register("e1".into(), caps_ef(true, false), HashSet::new(), port().0);
        assert!(
            pool.has_idle_eval_only_worker(),
            "idle eval-only worker present"
        );

        pool.assign_job("e1", "j1");
        assert!(
            !pool.has_idle_eval_only_worker(),
            "eval-only worker is busy"
        );
    }

    #[test]
    fn draining_eval_only_worker_does_not_count() {
        let mut pool = WorkerPool::new();
        pool.register("e1".into(), caps_ef(true, false), HashSet::new(), port().0);
        pool.mark_draining("e1");
        assert!(
            !pool.has_idle_eval_only_worker(),
            "draining worker excluded"
        );
    }

    #[test]
    fn test_register_and_is_connected() {
        let mut pool = WorkerPool::new();
        assert!(!pool.is_connected("w1"));
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        assert!(pool.is_connected("w1"));
        assert_eq!(pool.worker_count(), 1);
    }

    #[test]
    fn test_unregister_returns_assigned_jobs() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
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
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            vec!["x86_64-linux".into()],
            vec!["kvm".into()],
            4,
            8,
            16384,
            1200,
        );

        let workers = pool.all_workers();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].architectures, vec!["x86_64-linux"]);
        assert_eq!(workers[0].system_features, vec!["kvm"]);
        assert_eq!(workers[0].max_concurrent_builds, 4);

        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.cpu_count, 8);
        assert_eq!(view.ram_total_mb, 16384);
        assert_eq!(view.cpu_core_score, 1200);
    }

    #[test]
    fn reregister_preserves_reported_capabilities() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            vec!["x86_64-linux".into()],
            vec!["kvm".into()],
            4,
            8,
            16384,
            1200,
        );

        // A reconnect/re-auth re-registers without the worker re-sending caps;
        // architectures and sizing must survive so the worker stays matchable.
        pool.register("w1".into(), caps(), HashSet::new(), port().0);

        let workers = pool.all_workers();
        assert_eq!(workers[0].architectures, vec!["x86_64-linux"]);
        assert_eq!(workers[0].system_features, vec!["kvm"]);
        assert_eq!(workers[0].max_concurrent_builds, 4);
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.cpu_count, 8);
        assert_eq!(view.ram_total_mb, 16384);
    }

    #[test]
    fn test_update_metrics_updates_view() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities("w1", vec![], vec![], 1, 4, 8192, 1000);

        // Before any heartbeat the dynamic fields are absent, not zero.
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.cpu_usage_pct, None);
        assert_eq!(view.ram_free_mb, None);
        assert_eq!(view.disk_speed_mbps, None);
        assert_eq!(view.network_speed_mbps, None);

        pool.update_metrics("w1", 42.5, 3000, Some(550.0), Some(120.0));
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.cpu_usage_pct, Some(42.5));
        assert_eq!(view.ram_free_mb, Some(3000));
        assert_eq!(view.disk_speed_mbps, Some(550.0));
        assert_eq!(view.network_speed_mbps, Some(120.0));
        // Static caps survive a metrics update.
        assert_eq!(view.cpu_count, 4);
        assert_eq!(view.ram_total_mb, 8192);

        assert!(pool.metrics_for("unknown").is_none());
    }

    #[test]
    fn worker_caps_snapshot_is_coherent() {
        let mut pool = WorkerPool::new();
        pool.register(
            "w1".into(),
            GradientCapabilities {
                fetch: true,
                eval: true,
                ..Default::default()
            },
            HashSet::new(),
            port().0,
        );
        pool.update_capabilities(
            "w1",
            vec!["x86_64-linux".into()],
            vec!["kvm".into()],
            4,
            8,
            16384,
            1200,
        );
        pool.update_metrics("w1", 12.5, 9000, None, None);

        let caps = pool.worker_caps("w1").unwrap();
        assert!(caps.fetch);
        assert!(caps.capabilities.eval);
        assert_eq!(caps.architectures, vec!["x86_64-linux"]);
        assert_eq!(caps.system_features, vec!["kvm"]);
        assert_eq!(caps.metrics.unwrap().ram_free_mb, Some(9000));

        assert!(pool.worker_caps("unknown").is_none());
    }

    #[test]
    fn test_mark_draining() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);

        let info = &pool.all_workers()[0];
        assert!(!info.draining);

        pool.mark_draining("w1");
        let info = &pool.all_workers()[0];
        assert!(info.draining);
    }

    #[test]
    fn test_draining_worker_has_no_capacity() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities("w1", vec![], vec![], 10, 0, 0, 0);

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
        let peer_a = ProjectId::now_v7();
        let peer_b = ProjectId::now_v7();

        pool.register(
            "w1".into(),
            caps(),
            HashSet::from([peer_a, peer_b]),
            port().0,
        );
        let auth = pool.peer_auth_for("w1").unwrap();
        assert!(auth.contains(&peer_a));
        assert!(auth.contains(&peer_b));
        assert!(matches!(auth, PeerAuth::Restricted(_)));

        assert!(pool.peer_auth_for("w2").is_none());
    }

    #[test]
    fn test_update_authorized_peers() {
        let mut pool = WorkerPool::new();
        let peer_a = ProjectId::now_v7();
        let peer_b = ProjectId::now_v7();

        pool.register("w1".into(), caps(), HashSet::from([peer_a]), port().0);
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
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        assert!(matches!(pool.peer_auth_for("w1").unwrap(), PeerAuth::Open));
    }

    #[test]
    fn remove_sent_candidate_allows_reoffer() {
        // A build re-queued after a failed/rejected dispatch must lose its
        // sent flag so the next delta push re-offers it (workers re-score it).
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.mark_candidates_sent("w1", &["build:a".to_string(), "build:b".to_string()]);
        assert!(pool.sent_candidates_for("w1").unwrap().contains("build:a"));

        pool.remove_sent_candidate("build:a");
        let sent = pool.sent_candidates_for("w1").unwrap();
        assert!(!sent.contains("build:a"), "cleared job is re-offerable");
        assert!(sent.contains("build:b"), "other jobs stay sent");
    }

    #[test]
    fn build_capacity_strict_at_limit() {
        // Worker at exactly max_concurrent_builds must reject new builds.
        // Guards against `<` → `<=` off-by-one in `has_build_capacity`.
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities("w1", vec!["x86_64-linux".into()], vec![], 2, 0, 0, 0);

        assert!(pool.has_capacity("w1", &JobKind::Build), "0/2 has capacity");
        pool.assign_job("w1", "j1");
        assert!(pool.has_capacity("w1", &JobKind::Build), "1/2 has capacity");
        pool.assign_job("w1", "j2");
        assert!(
            !pool.has_capacity("w1", &JobKind::Build),
            "2/2 is at limit - must reject"
        );
        pool.release_job("w1", "j2");
        assert!(pool.has_capacity("w1", &JobKind::Build), "1/2 again");
    }

    #[test]
    fn test_assign_and_release_job() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);

        pool.assign_job("w1", "j1");
        assert_eq!(pool.all_workers()[0].assigned_job_count, 1);

        pool.assign_job("w1", "j2");
        assert_eq!(pool.all_workers()[0].assigned_job_count, 2);

        // Releasing one of two jobs leaves the worker busy → not idle.
        assert!(!pool.release_job("w1", "j1"));
        assert_eq!(pool.all_workers()[0].assigned_job_count, 1);

        // Releasing the last job makes the worker idle → dispatch may kick.
        assert!(pool.release_job("w1", "j2"));
        assert_eq!(pool.all_workers()[0].assigned_job_count, 0);
    }

    #[test]
    fn test_all_workers_info() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.register("w2".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities("w1", vec!["x86_64-linux".into()], vec![], 2, 0, 0, 0);
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
    fn test_all_workers_info_exposes_authorized_peers() {
        let mut pool = WorkerPool::new();
        let project_a = ProjectId::now_v7();
        let project_b = ProjectId::now_v7();

        // Restricted: worker authorized for project_a only.
        pool.register("w1".into(), caps(), HashSet::from([project_a]), port().0);
        // Open: no registrations.
        pool.register("w2".into(), caps(), HashSet::new(), port().0);

        let mut workers = pool.all_workers();
        workers.sort_by(|a, b| a.id.cmp(&b.id));

        let w1_peers = workers[0]
            .authorized_peers
            .as_ref()
            .expect("restricted worker should expose authorized peers");
        assert!(w1_peers.contains(&project_a));
        assert!(!w1_peers.contains(&project_b));

        assert!(
            workers[1].authorized_peers.is_none(),
            "open-mode worker reports None"
        );
    }

    #[test]
    fn test_request_reauth_notifies_connected_worker() {
        let mut pool = WorkerPool::new();
        let (session, mut signals) = port();
        pool.register("w1".into(), caps(), HashSet::new(), session);

        pool.request_reauth("w1");

        assert_eq!(signals.try_recv(), Ok(SessionSignal::Reauth));
    }

    #[test]
    fn send_abort_reaches_the_session_and_reports_unknown_workers() {
        let mut pool = WorkerPool::new();
        let (session, mut signals) = port();
        pool.register("w1".into(), caps(), HashSet::new(), session);

        assert!(pool.send_abort("w1", "j1".into(), "why".into()));
        assert!(!pool.send_abort("nope", "j1".into(), "why".into()));
        assert_eq!(
            signals.try_recv(),
            Ok(SessionSignal::Abort {
                job_id: "j1".into(),
                reason: "why".into()
            })
        );
    }

    #[test]
    fn test_request_reauth_noop_for_unknown_worker() {
        let pool = WorkerPool::new();
        pool.request_reauth("nonexistent");
    }

    #[test]
    fn stale_worker_ids_flags_only_silent_workers() {
        let mut pool = WorkerPool::new();
        let handle = pool.register("w1".into(), caps(), HashSet::new(), port().0);

        let now_ms = 1_000_000_000_000i64;
        let timeout_ms = 30_000i64;

        // Just heard from it: not stale.
        handle.store(now_ms, Ordering::Relaxed);
        assert!(pool.stale_worker_ids(now_ms, timeout_ms).is_empty());

        // Exactly at the deadline: still not stale (strict `>`).
        handle.store(now_ms - timeout_ms, Ordering::Relaxed);
        assert!(pool.stale_worker_ids(now_ms, timeout_ms).is_empty());

        // One millisecond past the deadline: stale.
        handle.store(now_ms - timeout_ms - 1, Ordering::Relaxed);
        assert_eq!(
            pool.stale_worker_ids(now_ms, timeout_ms),
            vec!["w1".to_string()]
        );

        // A freshly registered worker is stamped with `now`, so it is never
        // immediately stale against the real clock.
        pool.register("w2".into(), caps(), HashSet::new(), port().0);
        let real_now = gradient_types::now().and_utc().timestamp_millis();
        assert!(
            !pool
                .stale_worker_ids(real_now, timeout_ms)
                .contains(&"w2".to_string())
        );
    }
}
