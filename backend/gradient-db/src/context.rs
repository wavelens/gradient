/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::{Notify, mpsc};

use crate::pool::{WebDb, WorkerDb};
use crate::scheduling::startable_set::StartableSet;
use gradient_entity::ids::{CacheId, ProjectId};
use gradient_entity::{MappingError, StorePathMapper, StorePathMapping};
use gradient_storage::StorageCtx;
use gradient_types::{DerivationId, EvaluationId, RuntimeConfig};
use gradient_util::shutdown::Shutdown;

/// Probing is HTTP and must stay out of the graph writer's transaction.
/// A channel is keeping the network round trip off the one writer to the graph.
#[derive(Clone, Debug)]
pub struct ProbeRequests {
    sender: mpsc::UnboundedSender<Vec<DerivationId>>,
    inbox: Arc<Mutex<Option<mpsc::UnboundedReceiver<Vec<DerivationId>>>>>,
    staged: Option<Arc<Mutex<Vec<DerivationId>>>>,
    wake: Arc<Notify>,
}

impl Default for ProbeRequests {
    fn default() -> Self {
        let (sender, inbox) = mpsc::unbounded_channel();
        Self {
            sender,
            inbox: Arc::new(Mutex::new(Some(inbox))),
            staged: None,
            wake: Arc::default(),
        }
    }
}

impl ProbeRequests {
    /// The probe loop is not optional.
    /// Need is stopping at shared builds the probe has not answered.
    /// Nothing below an entry point is promoted without the loop.
    pub fn send(&self, derivations: Vec<DerivationId>) {
        if derivations.is_empty() {
            return;
        }

        match &self.staged {
            Some(staged) => lock(staged).extend(derivations),
            None => self.deliver(derivations),
        }
    }

    pub fn staged(&self) -> Self {
        Self {
            staged: Some(self.staged.clone().unwrap_or_default()),
            ..self.clone()
        }
    }

    pub fn unstaged(&self) -> Self {
        Self {
            staged: None,
            ..self.clone()
        }
    }

    pub fn publish(&self) {
        if let Some(staged) = &self.staged {
            let derivations = std::mem::take(&mut *lock(staged));
            if !derivations.is_empty() {
                self.deliver(derivations);
            }
        }
    }

    pub fn wake(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    pub fn take_inbox(&self) -> Option<mpsc::UnboundedReceiver<Vec<DerivationId>>> {
        self.inbox.lock().ok()?.take()
    }

    fn deliver(&self, derivations: Vec<DerivationId>) {
        let _ = self.sender.send(derivations);
        self.wake.notify_one();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DependencyOutputSigningRequest {
    pub parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign: HashSet<DerivationId>,
    pub dependency_derivation_ids_with_new_outputs: HashSet<DerivationId>,
    pub full_parent_build_check_requested: bool,
}

impl DependencyOutputSigningRequest {
    pub fn is_empty(&self) -> bool {
        !self.full_parent_build_check_requested
            && self
                .parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign
                .is_empty()
            && self.dependency_derivation_ids_with_new_outputs.is_empty()
    }

    fn merge(&mut self, other: Self) {
        self.parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign
            .extend(other.parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign);
        self.dependency_derivation_ids_with_new_outputs
            .extend(other.dependency_derivation_ids_with_new_outputs);
        self.full_parent_build_check_requested |= other.full_parent_build_check_requested;
    }
}

#[derive(Clone, Debug, Default)]
pub struct DependencyOutputPublishRequests {
    pending: Arc<Mutex<DependencyOutputSigningRequest>>,
    staged: Option<Arc<Mutex<DependencyOutputSigningRequest>>>,
}

impl DependencyOutputPublishRequests {
    pub fn request(&self, sweep: &Notify, request: DependencyOutputSigningRequest) {
        if request.is_empty() {
            return;
        }

        match &self.staged {
            Some(staged) => lock(staged).merge(request),
            None => {
                lock(&self.pending).merge(request);
                sweep.notify_one();
            }
        }
    }

    pub fn staged(&self) -> Self {
        Self {
            staged: Some(self.staged.clone().unwrap_or_default()),
            ..self.clone()
        }
    }

    pub fn unstaged(&self) -> Self {
        Self {
            staged: None,
            ..self.clone()
        }
    }

    pub fn publish(&self, sweep: &Notify) {
        if let Some(staged) = &self.staged {
            let request = std::mem::take(&mut *lock(staged));
            if !request.is_empty() {
                lock(&self.pending).merge(request);
                sweep.notify_one();
            }
        }
    }

    pub fn drain_pending_requests(&self) -> DependencyOutputSigningRequest {
        std::mem::take(&mut *lock(&self.pending))
    }

    pub fn put_back_unfinished_requests_without_waking_sweep(
        &self,
        unfinished: DependencyOutputSigningRequest,
    ) {
        lock(&self.pending).merge(unfinished);
    }
}

#[derive(Clone, Debug, Default)]
pub struct AlreadyStoredPushCacheQueryStorePathHashesByProject(
    Arc<Mutex<HashMap<ProjectId, HashSet<String>>>>,
);

impl AlreadyStoredPushCacheQueryStorePathHashesByProject {
    pub fn enqueue_store_path_hashes_for_project(
        &self,
        wake: &Notify,
        project: ProjectId,
        hashes: impl IntoIterator<Item = String>,
    ) {
        let mut hashes = hashes.into_iter().peekable();
        if hashes.peek().is_none() {
            return;
        }
        lock(&self.0).entry(project).or_default().extend(hashes);
        wake.notify_one();
    }

    pub fn list_projects_with_queued_store_path_hashes(&self) -> Vec<ProjectId> {
        lock(&self.0).keys().copied().collect()
    }

    pub fn take_queued_store_path_hashes(&self, project: ProjectId) -> HashSet<String> {
        lock(&self.0).remove(&project).unwrap_or_default()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Debug, Default)]
pub struct HeldEvaluations(Arc<Mutex<HashMap<EvaluationId, usize>>>);

#[must_use]
#[derive(Debug)]
pub struct Hold {
    held: HeldEvaluations,
    evaluation: EvaluationId,
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.held.release(self.evaluation);
    }
}

impl HeldEvaluations {
    pub async fn hold(&self, evaluation: EvaluationId) -> Hold {
        *self.held().entry(evaluation).or_default() += 1;
        Hold {
            held: self.clone(),
            evaluation,
        }
    }

    pub async fn holds(&self, evaluation: EvaluationId) -> bool {
        self.held().contains_key(&evaluation)
    }

    fn release(&self, evaluation: EvaluationId) {
        if let Entry::Occupied(mut count) = self.held().entry(evaluation) {
            *count.get_mut() -= 1;
            if *count.get() == 0 {
                count.remove();
            }
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, HashMap<EvaluationId, usize>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Wakeups {
    pub delivery: Arc<Notify>,
    pub eval_assign: Arc<Notify>,
    pub run_cache_maintenance_now: Arc<Notify>,
    pub run_signature_sweep_now: Arc<Notify>,
    pub run_signature_backfill_now: Arc<Notify>,
    pub run_signature_placeholder_insert_now: Arc<Notify>,
}

#[derive(Clone, Debug, Default)]
pub struct DbMemory {
    pub probe_requests: ProbeRequests,
    pub startable_set: StartableSet,
    pub held_evaluations: HeldEvaluations,
    pub dependency_output_publish_requests: DependencyOutputPublishRequests,
    pub store_path_hashes_awaiting_unsigned_signature_placeholder:
        AlreadyStoredPushCacheQueryStorePathHashesByProject,
}

impl DbMemory {
    pub fn staged(&self) -> Self {
        Self {
            startable_set: self.startable_set.staged(),
            probe_requests: self.probe_requests.staged(),
            dependency_output_publish_requests: self.dependency_output_publish_requests.staged(),
            ..self.clone()
        }
    }

    pub fn unstaged(&self) -> Self {
        Self {
            startable_set: self.startable_set.unstaged(),
            probe_requests: self.probe_requests.unstaged(),
            dependency_output_publish_requests: self.dependency_output_publish_requests.unstaged(),
            ..self.clone()
        }
    }
}

#[derive(Clone, Debug)]
pub struct DbContext {
    pub worker_db: WorkerDb,
    pub web_db: WebDb,
    pub config: Arc<RuntimeConfig>,
    pub storage: StorageCtx,
    pub shutdown: Shutdown,
    pub events: gradient_types::EventBus,
    pub wake: Wakeups,
    pub memory: DbMemory,
    pub store_path_mapping: Arc<dyn StorePathMapping>,
    pub user_supplied_url_http_client: gradient_util::http::HttpClientForUserSuppliedUrls,
}

impl DbContext {
    pub fn cache_mapper(&self, cache: CacheId) -> StorePathMapper {
        StorePathMapper::for_cache(Arc::clone(&self.store_path_mapping), cache)
    }

    pub async fn project_mapper(
        &self,
        project: ProjectId,
    ) -> Result<StorePathMapper, MappingError> {
        StorePathMapper::for_project(Arc::clone(&self.store_path_mapping), project).await
    }

    pub fn in_transaction(&self, tx: Arc<sea_orm::DatabaseTransaction>) -> DbContext {
        DbContext {
            worker_db: self.worker_db.in_transaction(tx),
            memory: self.memory.staged(),
            ..self.clone()
        }
    }

    pub fn detached(&self) -> DbContext {
        DbContext {
            worker_db: self.worker_db.detached(),
            memory: self.memory.unstaged(),
            ..self.clone()
        }
    }

    pub async fn record(&self, event: impl Into<gradient_types::Event>) {
        let event = event.into();
        let name = event.name();
        if let Err(e) =
            crate::deliveries::events::record(&self.worker_db, &self.events, event).await
        {
            tracing::error!(error = %e, event = %name, "failed to record an event");
        }
        self.wake.delivery.notify_one();
    }

    pub fn request_signature_sweep_to_check_parent_build_derivations(
        &self,
        parent_builds: impl IntoIterator<Item = DerivationId>,
    ) {
        self.memory.dependency_output_publish_requests.request(
            &self.wake.run_signature_sweep_now,
            DependencyOutputSigningRequest {
                parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign: parent_builds
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        );
    }

    pub fn request_parent_build_checks_for_dependency_derivations_with_fetchable_outputs(
        &self,
        dependencies: impl IntoIterator<Item = DerivationId>,
    ) {
        self.memory.dependency_output_publish_requests.request(
            &self.wake.run_signature_sweep_now,
            DependencyOutputSigningRequest {
                dependency_derivation_ids_with_new_outputs: dependencies.into_iter().collect(),
                ..Default::default()
            },
        );
    }

    pub fn queue_request_signature_sweep_to_check_all_parent_builds(&self) {
        self.memory.dependency_output_publish_requests.request(
            &self.wake.run_signature_sweep_now,
            DependencyOutputSigningRequest {
                full_parent_build_check_requested: true,
                ..Default::default()
            },
        );
    }

    pub fn queue_store_path_hashes_for_unsigned_signature_placeholder_insertion(
        &self,
        project: ProjectId,
        hashes: impl IntoIterator<Item = String>,
    ) {
        self.memory
            .store_path_hashes_awaiting_unsigned_signature_placeholder
            .enqueue_store_path_hashes_for_project(
                &self.wake.run_signature_placeholder_insert_now,
                project,
                hashes,
            );
    }

    pub async fn record_evaluation_created(&self, eval: &gradient_types::MEvaluation) {
        self.wake.eval_assign.notify_one();
        if let Some(event) = crate::deliveries::events::evaluation_created(eval) {
            self.record(event).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_evaluation_stays_held_until_its_last_hold_drops() {
        let held = HeldEvaluations::default();
        let evaluation = EvaluationId::now_v7();

        let first = held.hold(evaluation).await;
        let second = held.hold(evaluation).await;
        drop(first);
        assert!(held.holds(evaluation).await);

        drop(second);
        assert!(!held.holds(evaluation).await);
    }

    #[tokio::test]
    async fn a_probe_request_inside_a_transaction_waits_for_the_commit() {
        let requests = ProbeRequests::default();
        let mut inbox = requests.take_inbox().expect("a fresh channel has one");
        let wake = requests.wake();
        let derivation = DerivationId::now_v7();

        let transaction = requests.staged();
        transaction.send(vec![derivation]);
        assert!(
            inbox.try_recv().is_err(),
            "the probe must not read uncommitted rows"
        );

        transaction.publish();
        assert_eq!(inbox.try_recv().expect("published"), vec![derivation]);
        tokio::time::timeout(std::time::Duration::from_secs(1), wake.notified())
            .await
            .expect("the commit wakes the probe");
    }

    #[test]
    fn a_rolled_back_transaction_asks_the_probe_nothing() {
        let requests = ProbeRequests::default();
        let mut inbox = requests.take_inbox().expect("a fresh channel has one");

        requests.staged().send(vec![DerivationId::now_v7()]);
        requests.staged().publish();

        assert!(inbox.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_already_stored_push_cache_query_store_path_hashes_by_project_deduplicates_enqueues_notifies_wake_and_removes_project()
     {
        let queued = AlreadyStoredPushCacheQueryStorePathHashesByProject::default();
        let wake = Notify::new();
        let (project, other_project) = (ProjectId::now_v7(), ProjectId::now_v7());

        queued.enqueue_store_path_hashes_for_project(&wake, project, Vec::new());
        let empty_push_listed = queued.list_projects_with_queued_store_path_hashes();
        queued.enqueue_store_path_hashes_for_project(&wake, project, ["a".to_owned()]);
        queued.enqueue_store_path_hashes_for_project(
            &wake,
            project,
            ["a".to_owned(), "b".to_owned()],
        );
        queued.enqueue_store_path_hashes_for_project(&wake, other_project, ["c".to_owned()]);
        tokio::time::timeout(std::time::Duration::from_secs(1), wake.notified())
            .await
            .expect("a push wakes the placeholder pass");

        let taken = queued.take_queued_store_path_hashes(project);
        assert_eq!(
            (
                empty_push_listed,
                taken,
                queued.list_projects_with_queued_store_path_hashes()
            ),
            (
                Vec::new(),
                HashSet::from(["a".to_owned(), "b".to_owned()]),
                vec![other_project]
            )
        );
    }

    #[tokio::test]
    async fn a_dependency_output_signing_request_waits_for_the_commit_and_a_rollback_drops_it() {
        let requests = DependencyOutputPublishRequests::default();
        let sweep = Notify::new();
        let parent_build = DerivationId::now_v7();
        let request = DependencyOutputSigningRequest {
            parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign: HashSet::from([
                parent_build,
            ]),
            ..Default::default()
        };

        requests.staged().request(&sweep, request.clone());
        requests.staged().publish(&sweep);
        let rolled_back = requests.drain_pending_requests();

        let transaction = requests.staged();
        transaction.request(&sweep, request.clone());
        let before_commit = requests.drain_pending_requests();
        transaction.publish(&sweep);
        tokio::time::timeout(std::time::Duration::from_secs(1), sweep.notified())
            .await
            .expect("the commit wakes the signature sweep");

        assert_eq!(
            (
                rolled_back,
                before_commit,
                requests.drain_pending_requests()
            ),
            (
                DependencyOutputSigningRequest::default(),
                DependencyOutputSigningRequest::default(),
                request
            )
        );
    }
}
