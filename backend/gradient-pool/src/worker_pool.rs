/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use gradient_types::ids::ProjectId;
use gradient_wire::types::{GradientCapabilities, JobKind};

use crate::peer_auth::PeerAuth;
use crate::session_port::{SessionPort, SessionSignal};
use crate::worker_state::{Active, Draining, TypedWorker};

pub enum WorkerSlot {
    Active(TypedWorker<Active>),
    Draining(TypedWorker<Draining>),
}

impl WorkerSlot {
    fn shared(&self) -> &crate::worker_state::WorkerShared {
        match self {
            Self::Active(w) => w,
            Self::Draining(w) => w,
        }
    }

    fn shared_mut(&mut self) -> &mut crate::worker_state::WorkerShared {
        match self {
            Self::Active(w) => w,
            Self::Draining(w) => w,
        }
    }

    pub fn is_draining(&self) -> bool {
        matches!(self, Self::Draining(_))
    }
}

impl std::fmt::Debug for WorkerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active(w) => f.debug_tuple("Active").field(&w.shared).finish(),
            Self::Draining(w) => f.debug_tuple("Draining").field(&w.shared).finish(),
        }
    }
}

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

    /// The prior connection's sizing is carried forward because a reconnect may arrive without
    /// fresh `WorkerCapabilities`. The `last_seen` Arc is reused for readers still holding the old
    /// handle.
    pub fn register(
        &mut self,
        id: String,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<ProjectId>,
        session: Arc<dyn SessionPort>,
    ) -> Arc<AtomicI64> {
        let prior = self.workers.get(&id).map(|slot| {
            (
                slot.shared().profile(),
                Arc::clone(&slot.shared().last_seen),
            )
        });
        let mut worker = TypedWorker::<Active>::new(capabilities, authorized_peers, session);
        if let Some((profile, last_seen)) = prior {
            worker.apply_profile(profile);
            last_seen.store(
                gradient_types::now().and_utc().timestamp_millis(),
                Ordering::Relaxed,
            );
            worker.last_seen = last_seen;
        }

        let last_seen = Arc::clone(&worker.last_seen);
        self.workers.insert(id, WorkerSlot::Active(worker));
        last_seen
    }

    pub fn request_reauth(&self, worker_id: &str) {
        if let Some(slot) = self.workers.get(worker_id) {
            slot.shared().session.signal(SessionSignal::Reauth);
        }
    }

    pub fn signal(&self, worker_id: &str, signal: SessionSignal) -> bool {
        match self.workers.get(worker_id) {
            Some(slot) => {
                slot.shared().session.signal(signal);
                true
            }
            None => false,
        }
    }

    pub fn send_abort(&self, worker_id: &str, job_id: String, reason: String) -> bool {
        self.signal(worker_id, SessionSignal::Abort { job_id, reason })
    }

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

    pub fn peer_auth_for(&self, id: &str) -> Option<&PeerAuth> {
        self.workers.get(id).map(|slot| &slot.shared().peer_auth)
    }

    pub fn gradient_caps_for(&self, id: &str) -> Option<GradientCapabilities> {
        self.workers
            .get(id)
            .map(|slot| slot.shared().capabilities.clone())
    }

    pub fn worker_caps(&self, id: &str) -> Option<crate::WorkerCaps> {
        self.workers.get(id).map(|slot| {
            let s = slot.shared();
            crate::WorkerCaps {
                fetch: s.capabilities.fetch,
                architectures: s.architectures.clone(),
                system_features: s.system_features.clone(),
                capabilities: s.capabilities.clone(),
                metrics: self.metrics_for(id),
                zone: s.zone.clone(),
                endpoint: s.endpoint.clone(),
            }
        })
    }

    pub fn update_capabilities(&mut self, id: &str, profile: crate::WorkerProfile) {
        if let Some(slot) = self.workers.get_mut(id) {
            slot.shared_mut().apply_profile(profile);
        }
    }

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

    pub fn metrics_for(&self, id: &str) -> Option<crate::score::WorkerMetricsView> {
        self.workers.get(id).map(|slot| {
            let s = slot.shared();
            crate::score::WorkerMetricsView {
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

    /// The session must be closed too. A dropped slot alone would leave an untracked live socket,
    /// and the evicted worker would never reconnect.
    pub fn unregister(&mut self, id: &str) -> Vec<String> {
        self.workers
            .remove(id)
            .map(|slot| {
                let shared = slot.shared();
                shared.session.signal(SessionSignal::Close {
                    reason: "unregistered by the scheduler".into(),
                });
                shared.assigned_jobs.iter().cloned().collect()
            })
            .unwrap_or_default()
    }

    pub fn aggregate(&self) -> crate::Aggregate {
        crate::aggregate(
            self.workers
                .values()
                .filter(|slot| !slot.is_draining())
                .map(WorkerSlot::shared),
        )
    }

    pub fn mark_draining(&mut self, id: &str) {
        if let Some(slot) = self.workers.remove(id) {
            let new_slot = match slot {
                WorkerSlot::Active(w) => WorkerSlot::Draining(w.into_draining()),
                already_draining => already_draining,
            };
            self.workers.insert(id.to_owned(), new_slot);
        }
    }

    pub fn set_sent_candidates(&mut self, worker_id: &str, job_ids: HashSet<String>) {
        if let Some(slot) = self.workers.get_mut(worker_id) {
            slot.shared_mut().sent_candidates = job_ids;
        }
    }

    pub fn remove_sent_candidate(&mut self, job_id: &str) {
        for slot in self.workers.values_mut() {
            slot.shared_mut().sent_candidates.remove(job_id);
        }
    }

    pub fn sent_candidates_for(&self, worker_id: &str) -> Option<&HashSet<String>> {
        self.workers
            .get(worker_id)
            .map(|slot| &slot.shared().sent_candidates)
    }

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

    pub fn worker_counts(&self) -> (u32, u32) {
        let total = self.workers.len() as u32;
        let idle = self
            .workers
            .values()
            .filter(|slot| slot.shared().assigned_jobs.is_empty())
            .count() as u32;
        (total, idle)
    }

    pub fn mean_cpu_core_score(&self) -> Option<f64> {
        let scores: Vec<f64> = self
            .workers
            .values()
            .map(|slot| slot.shared().cpu_core_score)
            .filter(|s| *s > 0)
            .map(f64::from)
            .collect();
        (!scores.is_empty()).then(|| scores.iter().sum::<f64>() / scores.len() as f64)
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerInfo {
    pub id: String,
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
    pub authorized_peers: Option<HashSet<ProjectId>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_auth::PeerAuth;
    use crate::session_port::{SessionPort, SessionSignal};
    use crate::worker_state::WorkerProfile;
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
    fn mean_cpu_core_score_skips_workers_without_one() {
        let mut pool = WorkerPool::new();
        assert_eq!(pool.mean_cpu_core_score(), None);
        for (id, score) in [("w1", 1_000), ("w2", 3_000), ("w3", 0)] {
            pool.register(id.into(), caps(), HashSet::new(), port().0);
            pool.update_capabilities(
                id,
                WorkerProfile {
                    max_concurrent_builds: 1,
                    cpu_count: 1,
                    ram_total_mb: 1,
                    cpu_core_score: score,
                    ..Default::default()
                },
            );
        }
        assert_eq!(pool.mean_cpu_core_score(), Some(2_000.0));
    }

    #[test]
    fn reregister_preserves_reported_capabilities() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                system_features: vec!["kvm".into()],
                max_concurrent_builds: 4,
                cpu_count: 8,
                ram_total_mb: 16384,
                cpu_core_score: 1200,
                zone: Some("fra1".into()),
                endpoint: Some("10.0.0.7:7000".into()),
            },
        );

        pool.register("w1".into(), caps(), HashSet::new(), port().0);

        let workers = pool.all_workers();
        assert_eq!(workers[0].architectures, vec!["x86_64-linux"]);
        assert_eq!(workers[0].system_features, vec!["kvm"]);
        assert_eq!(workers[0].max_concurrent_builds, 4);
        let shared = pool.workers["w1"].shared();
        assert_eq!(shared.zone.as_deref(), Some("fra1"));
        assert_eq!(shared.endpoint.as_deref(), Some("10.0.0.7:7000"));
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.cpu_count, 8);
        assert_eq!(view.ram_total_mb, 16384);
    }

    #[test]
    fn an_empty_zone_is_no_zone() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                zone: Some(String::new()),
                endpoint: Some(String::new()),
                ..Default::default()
            },
        );

        let shared = pool.workers["w1"].shared();
        assert_eq!(shared.zone, None);
        assert_eq!(shared.endpoint, None);
    }

    #[test]
    fn worker_caps_carry_the_zone() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                zone: Some("fra1".into()),
                ..Default::default()
            },
        );

        assert_eq!(
            pool.worker_caps("w1").unwrap().zone.as_deref(),
            Some("fra1")
        );
    }

    #[test]
    fn test_update_metrics_updates_view() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                max_concurrent_builds: 1,
                cpu_count: 4,
                ram_total_mb: 8192,
                cpu_core_score: 1000,
                ..Default::default()
            },
        );

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
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                system_features: vec!["kvm".into()],
                max_concurrent_builds: 4,
                cpu_count: 8,
                ram_total_mb: 16384,
                cpu_core_score: 1200,
                ..Default::default()
            },
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
    fn test_draining_worker_has_no_capacity() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                max_concurrent_builds: 10,
                ..Default::default()
            },
        );

        assert!(pool.has_capacity("w1", &JobKind::Build));
        assert!(pool.has_capacity("w1", &JobKind::Flake));

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
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.set_sent_candidates("w1", HashSet::from(["build:a".into(), "build:b".into()]));
        assert!(pool.sent_candidates_for("w1").unwrap().contains("build:a"));

        pool.remove_sent_candidate("build:a");
        let sent = pool.sent_candidates_for("w1").unwrap();
        assert!(!sent.contains("build:a"), "cleared job is re-offerable");
        assert!(sent.contains("build:b"), "other jobs stay sent");
    }

    #[test]
    fn build_capacity_strict_at_limit() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                max_concurrent_builds: 2,
                ..Default::default()
            },
        );

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

        assert!(!pool.release_job("w1", "j1"));
        assert_eq!(pool.all_workers()[0].assigned_job_count, 1);

        assert!(pool.release_job("w1", "j2"));
        assert_eq!(pool.all_workers()[0].assigned_job_count, 0);
    }

    #[test]
    fn test_all_workers_info() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), HashSet::new(), port().0);
        pool.register("w2".into(), caps(), HashSet::new(), port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                max_concurrent_builds: 2,
                ..Default::default()
            },
        );
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

        pool.register("w1".into(), caps(), HashSet::from([project_a]), port().0);
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
    fn unregister_closes_the_evicted_session() {
        let mut pool = WorkerPool::new();
        let (session, mut signals) = port();
        pool.register("w1".into(), caps(), HashSet::new(), session);

        pool.unregister("w1");

        assert_eq!(
            signals.try_recv(),
            Ok(SessionSignal::Close {
                reason: "unregistered by the scheduler".into()
            }),
            "an evicted worker must be told, or it keeps a session the pool no longer knows about"
        );
    }

    #[test]
    fn unregister_of_an_unknown_worker_signals_nothing() {
        let mut pool = WorkerPool::new();
        assert!(pool.unregister("nope").is_empty());
    }

    #[test]
    fn stale_worker_ids_flags_only_silent_workers() {
        let mut pool = WorkerPool::new();
        let handle = pool.register("w1".into(), caps(), HashSet::new(), port().0);

        let now_ms = 1_000_000_000_000i64;
        let timeout_ms = 30_000i64;

        handle.store(now_ms, Ordering::Relaxed);
        assert!(pool.stale_worker_ids(now_ms, timeout_ms).is_empty());

        handle.store(now_ms - timeout_ms, Ordering::Relaxed);
        assert!(pool.stale_worker_ids(now_ms, timeout_ms).is_empty());

        handle.store(now_ms - timeout_ms - 1, Ordering::Relaxed);
        assert_eq!(
            pool.stale_worker_ids(now_ms, timeout_ms),
            vec!["w1".to_string()]
        );

        pool.register("w2".into(), caps(), HashSet::new(), port().0);
        let real_now = gradient_types::now().and_utc().timestamp_millis();
        assert!(
            !pool
                .stale_worker_ids(real_now, timeout_ms)
                .contains(&"w2".to_string())
        );
    }

    #[test]
    fn reregistering_a_worker_keeps_the_liveness_handle_its_reader_holds() {
        let mut pool = WorkerPool::new();
        let first = pool.register("w1".into(), caps(), HashSet::new(), port().0);
        let second = pool.register("w1".into(), caps(), HashSet::new(), port().0);
        assert!(
            Arc::ptr_eq(&first, &second),
            "a re-registration must hand back the Arc the prior reader still stamps"
        );

        let now_ms = 1_000_000_000_000i64;
        let timeout_ms = 30_000i64;

        first.store(now_ms - timeout_ms - 1, Ordering::Relaxed);
        assert_eq!(
            pool.stale_worker_ids(now_ms, timeout_ms),
            vec!["w1".to_string()],
            "the pool reads the value stamped through the first handle"
        );

        first.store(now_ms, Ordering::Relaxed);
        assert!(
            pool.stale_worker_ids(now_ms, timeout_ms).is_empty(),
            "a stamp through the first handle keeps the worker live"
        );

        let other = pool.register("w2".into(), caps(), HashSet::new(), port().0);
        assert!(
            !Arc::ptr_eq(&first, &other),
            "a different worker gets its own handle"
        );
    }
}
