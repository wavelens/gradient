/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-memory registry of connected proto workers.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::messages::GradientCapabilities;

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
    ) {
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
            },
        );
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
