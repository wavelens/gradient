/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-memory registry of connected proto workers.

use std::collections::{HashMap, HashSet};

use crate::messages::{Architecture, GradientCapabilities};

/// Metadata for a single connected worker.
#[derive(Debug)]
pub struct ConnectedWorker {
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<Architecture>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_jobs: HashSet<String>,
    pub draining: bool,
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

    pub fn register(&mut self, id: String, capabilities: GradientCapabilities) {
        self.workers.insert(
            id,
            ConnectedWorker {
                capabilities,
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: 1,
                assigned_jobs: HashSet::new(),
                draining: false,
            },
        );
    }

    pub fn update_capabilities(
        &mut self,
        id: &str,
        architectures: Vec<Architecture>,
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
}
