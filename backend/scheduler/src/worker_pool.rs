/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-memory registry of connected proto workers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::Notify;
use uuid::Uuid;

use gradient_core::types::proto::GradientCapabilities;

/// Metadata for a single connected worker.
#[derive(Debug)]
pub struct ConnectedWorker {
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_jobs: HashSet<String>,
    pub draining: bool,
    /// Peer IDs (org/cache/proxy UUIDs) this worker is authorized for.
    /// Empty means no peers registered this worker (open/discoverable mode).
    pub authorized_peers: HashSet<Uuid>,
    /// Signalled by the API when registrations change and the worker should
    /// re-authenticate without disconnecting.
    pub reauth_notify: Arc<Notify>,
}

/// In-memory registry of all currently connected workers.
#[derive(Debug, Default)]
pub struct WorkerPool {
    workers: HashMap<String, ConnectedWorker>,
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
    ) -> Arc<Notify> {
        let notify = Arc::new(Notify::new());
        self.workers.insert(
            id,
            ConnectedWorker {
                capabilities,
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: 1,
                assigned_jobs: HashSet::new(),
                draining: false,
                authorized_peers,
                reauth_notify: Arc::clone(&notify),
            },
        );
        notify
    }

    /// Signal a connected worker that its registrations have changed and it
    /// should re-authenticate.  No-op if the worker is not connected.
    pub fn request_reauth(&self, worker_id: &str) {
        if let Some(w) = self.workers.get(worker_id) {
            w.reauth_notify.notify_one();
        }
    }

    pub fn update_authorized_peers(&mut self, id: &str, authorized_peers: HashSet<Uuid>) {
        if let Some(w) = self.workers.get_mut(id) {
            w.authorized_peers = authorized_peers;
        }
    }

    /// Returns the authorized peer set for a worker, or `None` if the worker
    /// is not connected. An empty set means no peers registered this worker
    /// (open/discoverable mode — all jobs visible).
    pub fn authorized_peers_for(&self, id: &str) -> Option<&HashSet<Uuid>> {
        self.workers.get(id).map(|w| &w.authorized_peers)
    }

    pub fn update_capabilities(
        &mut self,
        id: &str,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
    ) {
        if let Some(w) = self.workers.get_mut(id) {
            w.architectures = architectures;
            w.system_features = system_features;
            w.max_concurrent_builds = max_concurrent_builds;
        }
    }

    pub fn unregister(&mut self, id: &str) -> Vec<String> {
        self.workers
            .remove(id)
            .map(|w| w.assigned_jobs.into_iter().collect())
            .unwrap_or_default()
    }

    pub fn mark_draining(&mut self, id: &str) {
        if let Some(w) = self.workers.get_mut(id) {
            w.draining = true;
        }
    }

    pub fn assign_job(&mut self, worker_id: &str, job_id: &str) {
        if let Some(w) = self.workers.get_mut(worker_id) {
            w.assigned_jobs.insert(job_id.to_owned());
        }
    }

    pub fn release_job(&mut self, worker_id: &str, job_id: &str) {
        if let Some(w) = self.workers.get_mut(worker_id) {
            w.assigned_jobs.remove(job_id);
        }
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub fn all_workers(&self) -> Vec<WorkerInfo> {
        self.workers
            .iter()
            .map(|(id, w)| WorkerInfo {
                id: id.clone(),
                architectures: w.architectures.clone(),
                system_features: w.system_features.clone(),
                max_concurrent_builds: w.max_concurrent_builds,
                assigned_job_count: w.assigned_jobs.len(),
                draining: w.draining,
            })
            .collect()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerInfo {
    pub id: String,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_authorized_peers_for() {
        let mut pool = WorkerPool::new();
        let peer_a = Uuid::new_v4();
        let peer_b = Uuid::new_v4();

        pool.register("w1".into(), caps(), HashSet::from([peer_a, peer_b]));
        let peers = pool.authorized_peers_for("w1").unwrap();
        assert!(peers.contains(&peer_a));
        assert!(peers.contains(&peer_b));
        assert_eq!(peers.len(), 2);

        // Unknown worker returns None.
        assert!(pool.authorized_peers_for("w2").is_none());
    }

    #[test]
    fn test_update_authorized_peers() {
        let mut pool = WorkerPool::new();
        let peer_a = Uuid::new_v4();
        let peer_b = Uuid::new_v4();

        pool.register("w1".into(), caps(), HashSet::from([peer_a]));
        assert_eq!(pool.authorized_peers_for("w1").unwrap().len(), 1);

        pool.update_authorized_peers("w1", HashSet::from([peer_a, peer_b]));
        assert_eq!(pool.authorized_peers_for("w1").unwrap().len(), 2);
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
        let notify = pool.register("w1".into(), caps(), HashSet::new());

        pool.request_reauth("w1");

        // After request_reauth, the notify should fire immediately.
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
        // Should not panic for unknown worker.
        pool.request_reauth("nonexistent");
    }
}
