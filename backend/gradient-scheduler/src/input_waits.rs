/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    pub input_fetch_handed_to_worker_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnassignedFetch {
    pub fetch_key: String,
    pub project: ProjectId,
    pub request: String,
    pub needs_secrets: bool,
    pub excluded: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFetchProjectGitCredentials {
    FetchWithoutProjectCredentials,
    FetchWithProjectCredentials,
    HttpsCredentialsWithheldForUnapprovedForkPullRequest,
}

#[derive(Debug)]
struct InputWait {
    request: String,
    project: ProjectId,
    project_git_credentials: InputFetchProjectGitCredentials,
    content_hash_store_path: Option<String>,
    waiters: Vec<InputWaiter>,
    fetch: Option<AssignedFetch>,
    excluded: HashSet<String>,
    announced: HashSet<EvaluationId>,
    input_wait_created_at: Instant,
}

#[derive(Debug, Clone)]
pub struct NewInputWait {
    pub fetch_key: String,
    pub request: String,
    pub project: ProjectId,
    pub project_git_credentials: InputFetchProjectGitCredentials,
    pub content_hash_store_path: Option<String>,
    pub waiter: InputWaiter,
}

#[derive(Clone, Debug, Default)]
pub struct InputWaits(Arc<Mutex<HashMap<String, InputWait>>>);

impl InputWaits {
    pub fn register(&self, new: NewInputWait) {
        let mut waits = self.0.lock();
        for wait in waits.values_mut() {
            wait.waiters.retain(|w| {
                !(w.job_id == new.waiter.job_id && w.request_id == new.waiter.request_id)
            });
        }
        waits
            .entry(new.fetch_key)
            .or_insert_with(|| InputWait {
                request: new.request,
                project: new.project,
                project_git_credentials: new.project_git_credentials,
                content_hash_store_path: new.content_hash_store_path,
                waiters: Vec::new(),
                fetch: None,
                excluded: HashSet::new(),
                announced: HashSet::new(),
                input_wait_created_at: Instant::now(),
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
                needs_secrets: wait.project_git_credentials
                    == InputFetchProjectGitCredentials::FetchWithProjectCredentials,
                excluded: wait.excluded.clone(),
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

    pub fn take_fetched(
        &self,
        worker: &str,
        fetch_id: &str,
    ) -> Option<(InputFetchProjectGitCredentials, Vec<InputWaiter>)> {
        let mut waits = self.0.lock();
        let key = key_of_fetch(&waits, worker, fetch_id)?;
        waits
            .remove(&key)
            .map(|w| (w.project_git_credentials, w.waiters))
    }

    pub fn unplace(&self, worker: &str, fetch_id: &str) -> Option<String> {
        let mut waits = self.0.lock();
        let key = key_of_fetch(&waits, worker, fetch_id)?;
        let wait = waits.get_mut(&key)?;
        wait.fetch = None;
        wait.excluded.insert(worker.to_owned());
        Some(key)
    }

    pub fn project_and_content_pinned_store_path_for_pinned_wait(
        &self,
        worker: &str,
        fetch_id: &str,
    ) -> Option<(ProjectId, String)> {
        let waits = self.0.lock();
        let wait = waits.get(&key_of_fetch(&waits, worker, fetch_id)?)?;
        Some((wait.project, wait.content_hash_store_path.clone()?))
    }

    pub fn assigned_fetches_held_at_least_timeout(
        &self,
        timeout: Duration,
        now: Instant,
    ) -> Vec<AssignedFetch> {
        self.0
            .lock()
            .values()
            .filter_map(|w| w.fetch.clone())
            .filter(|f| now.saturating_duration_since(f.input_fetch_handed_to_worker_at) >= timeout)
            .collect()
    }

    pub fn remove_waits_without_fetch_created_at_least_timeout_before_and_return_input_requests_and_waiters(
        &self,
        timeout: Duration,
        now: Instant,
    ) -> Vec<(String, Vec<InputWaiter>)> {
        let mut waits = self.0.lock();
        let expired: Vec<String> = waits
            .iter()
            .filter(|(_, w)| {
                w.fetch.is_none()
                    && now.saturating_duration_since(w.input_wait_created_at) >= timeout
            })
            .map(|(key, _)| key.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|key| waits.remove(&key))
            .map(|w| (w.request, w.waiters))
            .collect()
    }

    pub fn remove_wait_without_fetch_by_key_and_return_waiters(
        &self,
        fetch_key: &str,
    ) -> Vec<InputWaiter> {
        let mut waits = self.0.lock();
        if waits.get(fetch_key).is_none_or(|w| w.fetch.is_some()) {
            return Vec::new();
        }
        waits
            .remove(fetch_key)
            .map(|w| w.waiters)
            .unwrap_or_default()
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
            wait.excluded.remove(worker);
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

    pub fn announce_unplaced(&self, fetch_key: &str) -> Vec<EvaluationId> {
        let mut waits = self.0.lock();
        let Some(wait) = waits.get_mut(fetch_key).filter(|w| w.fetch.is_none()) else {
            return Vec::new();
        };
        let mut fresh: Vec<EvaluationId> = wait
            .waiters
            .iter()
            .map(|waiter| waiter.evaluation)
            .filter(|evaluation| wait.announced.insert(*evaluation))
            .collect();
        fresh.dedup();
        fresh
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
