/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gradient_types::ids::{EvaluationId, ProjectId};
use gradient_util::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputWaiter {
    pub worker: String,
    pub job_id: String,
    pub request_id: String,
    pub evaluation: EvaluationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedFetch {
    pub fetch_id: String,
    pub worker: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnassignedFetch {
    pub fetch_key: String,
    pub project: ProjectId,
    pub request: String,
    pub needs_secrets: bool,
}

#[derive(Debug)]
struct InputWait {
    request: String,
    project: ProjectId,
    needs_secrets: bool,
    waiters: Vec<InputWaiter>,
    fetch: Option<AssignedFetch>,
}

#[derive(Debug, Clone)]
pub struct NewInputWait {
    pub fetch_key: String,
    pub request: String,
    pub project: ProjectId,
    pub needs_secrets: bool,
    pub waiter: InputWaiter,
}

#[derive(Clone, Debug, Default)]
pub struct InputWaits(Arc<Mutex<HashMap<String, InputWait>>>);

impl InputWaits {
    pub fn register(&self, new: NewInputWait) {
        self.0
            .lock()
            .entry(new.fetch_key)
            .or_insert_with(|| InputWait {
                request: new.request,
                project: new.project,
                needs_secrets: new.needs_secrets,
                waiters: Vec::new(),
                fetch: None,
            })
            .waiters
            .push(new.waiter);
    }

    pub fn unassigned(&self) -> Vec<UnassignedFetch> {
        let mut unassigned: Vec<UnassignedFetch> = self
            .0
            .lock()
            .iter()
            .filter(|(_, wait)| wait.fetch.is_none())
            .map(|(key, wait)| UnassignedFetch {
                fetch_key: key.clone(),
                project: wait.project,
                request: wait.request.clone(),
                needs_secrets: wait.needs_secrets,
            })
            .collect();
        unassigned.sort_by(|a, b| a.fetch_key.cmp(&b.fetch_key));
        unassigned
    }

    pub fn assigned_per_worker(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for fetch in self.0.lock().values().filter_map(|w| w.fetch.as_ref()) {
            *counts.entry(fetch.worker.clone()).or_default() += 1;
        }
        counts
    }

    pub fn assign(&self, fetch_key: &str, fetch: AssignedFetch) -> bool {
        match self.0.lock().get_mut(fetch_key) {
            Some(wait) if wait.fetch.is_none() => {
                wait.fetch = Some(fetch);
                true
            }
            _ => false,
        }
    }

    pub fn take_fetched(&self, worker: &str, fetch_id: &str) -> Vec<InputWaiter> {
        let mut waits = self.0.lock();
        let Some(key) = key_of_fetch(&waits, worker, fetch_id) else {
            return Vec::new();
        };
        waits.remove(&key).map(|w| w.waiters).unwrap_or_default()
    }

    pub fn waiters_of_fetch(&self, worker: &str, fetch_id: &str) -> Vec<InputWaiter> {
        let waits = self.0.lock();
        key_of_fetch(&waits, worker, fetch_id)
            .and_then(|key| waits.get(&key))
            .map(|w| w.waiters.clone())
            .unwrap_or_default()
    }

    pub fn release_worker(&self, worker: &str) {
        let mut waits = self.0.lock();
        for wait in waits.values_mut() {
            wait.waiters.retain(|w| w.worker != worker);
            if wait.fetch.as_ref().is_some_and(|f| f.worker == worker) {
                wait.fetch = None;
            }
        }
        waits.retain(|_, wait| !wait.waiters.is_empty());
    }

    pub fn drop_evaluations(&self, evaluations: &HashSet<EvaluationId>) -> usize {
        let mut waits = self.0.lock();
        let mut dropped = 0;
        for wait in waits.values_mut() {
            let before = wait.waiters.len();
            wait.waiters
                .retain(|w| !evaluations.contains(&w.evaluation));
            dropped += before - wait.waiters.len();
        }
        waits.retain(|_, wait| !wait.waiters.is_empty());
        dropped
    }

    pub fn waiting_evaluations(&self) -> HashSet<EvaluationId> {
        self.0
            .lock()
            .values()
            .flat_map(|w| w.waiters.iter().map(|waiter| waiter.evaluation))
            .collect()
    }

    pub fn unassigned_evaluations(&self) -> HashSet<EvaluationId> {
        self.0
            .lock()
            .values()
            .filter(|w| w.fetch.is_none())
            .flat_map(|w| w.waiters.iter().map(|waiter| waiter.evaluation))
            .collect()
    }

    #[cfg(test)]
    pub fn waiting_requests(&self) -> usize {
        self.0.lock().values().map(|w| w.waiters.len()).sum()
    }
}

pub(crate) fn input_fetch_key(fetch_id: &str) -> String {
    format!("input:{fetch_id}")
}

fn key_of_fetch(
    waits: &HashMap<String, InputWait>,
    worker: &str,
    fetch_id: &str,
) -> Option<String> {
    waits
        .iter()
        .find(|(_, wait)| {
            wait.fetch
                .as_ref()
                .is_some_and(|f| f.fetch_id == fetch_id && f.worker == worker)
        })
        .map(|(key, _)| key.clone())
}
