/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The core keeps its pool in actor state, so a respawned core starts empty. The latest worker
//! reports live here, outside the core and a reattach hands them to the new core.

use std::collections::HashMap;
use std::sync::Arc;

use gradient_pool::ProjectAccess;
use gradient_util::sync::Mutex;
use gradient_wire::types::GradientCapabilities;

use crate::actor::{WorkerCapabilities, WorkerMetrics};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkerProjectAccessAndCapabilities {
    pub project_access: ProjectAccess,
    pub capabilities: GradientCapabilities,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkerReports {
    pub capabilities: Option<WorkerCapabilities>,
    pub metrics: Option<WorkerMetrics>,
    pub authorization: Option<WorkerProjectAccessAndCapabilities>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct WorkerMemory(Arc<Mutex<HashMap<String, WorkerReports>>>);

impl WorkerMemory {
    pub fn update(&self, worker: &str, change: impl FnOnce(&mut WorkerReports)) {
        change(self.0.lock().entry(worker.to_owned()).or_default());
    }

    pub fn reports(&self, worker: &str) -> WorkerReports {
        self.0.lock().get(worker).cloned().unwrap_or_default()
    }

    pub fn forget(&self, worker: &str) {
        self.0.lock().remove(worker);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gradient_types::ids::ProjectId;

    use super::*;

    #[test]
    fn the_latest_report_wins_until_the_worker_is_forgotten() {
        let memory = WorkerMemory::default();
        let narrowed = WorkerProjectAccessAndCapabilities {
            project_access: ProjectAccess::OnlyListedProjects(HashSet::from([ProjectId::now_v7()])),
            capabilities: GradientCapabilities::default(),
        };
        memory.update("w1", |r| {
            r.authorization = Some(WorkerProjectAccessAndCapabilities {
                project_access: ProjectAccess::no_project_access(),
                capabilities: GradientCapabilities::default(),
            })
        });
        memory.update("w1", |r| r.authorization = Some(narrowed.clone()));

        assert_eq!(memory.reports("w1").authorization, Some(narrowed));
        memory.forget("w1");
        assert!(memory.reports("w1").authorization.is_none());
    }
}
