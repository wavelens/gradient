/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerJob<'a> {
    Evaluation,
    Build,
    Download,
    Exec,
    InputFetch,
    Substitution {
        verifying_keys: &'a [Option<String>],
    },
}

pub trait WorkerJobPolicy: Send + Sync + fmt::Debug {
    fn allows(&self, worker: &str, job: WorkerJob<'_>) -> bool;
}

#[derive(Debug)]
pub struct EveryJobAllowed;

impl WorkerJobPolicy for EveryJobAllowed {
    fn allows(&self, _worker: &str, _job: WorkerJob<'_>) -> bool {
        true
    }
}

#[derive(Clone, Debug)]
pub struct AllowedJobs {
    worker: String,
    policy: Arc<dyn WorkerJobPolicy>,
}

impl AllowedJobs {
    pub fn of(worker: &str, policy: &Arc<dyn WorkerJobPolicy>) -> Self {
        Self {
            worker: worker.to_owned(),
            policy: Arc::clone(policy),
        }
    }

    pub fn contains(&self, job: WorkerJob<'_>) -> bool {
        self.policy.allows(&self.worker, job)
    }
}

impl Default for AllowedJobs {
    fn default() -> Self {
        Self {
            worker: String::new(),
            policy: Arc::new(EveryJobAllowed),
        }
    }
}
