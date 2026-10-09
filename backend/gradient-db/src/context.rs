/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::{Notify, mpsc};

use crate::pool::{WebDb, WorkerDb};
use crate::scheduling::startable_set::StartableSet;
use gradient_storage::StorageCtx;
use gradient_types::{DerivationId, EvaluationId, RuntimeConfig};
use gradient_util::shutdown::Shutdown;

/// Probing is HTTP and must stay out of the graph writer's transaction.
/// A channel is keeping the network round trip off the one writer to the graph.
#[derive(Clone, Debug, Default)]
pub struct ProbeRequests {
    sender: Option<mpsc::UnboundedSender<Vec<DerivationId>>>,
    inbox: Arc<Mutex<Option<mpsc::UnboundedReceiver<Vec<DerivationId>>>>>,
    staged: Option<Arc<Mutex<Vec<DerivationId>>>>,
    wake: Arc<Notify>,
}

impl ProbeRequests {
    pub fn channel() -> Self {
        let (sender, inbox) = mpsc::unbounded_channel();
        Self {
            sender: Some(sender),
            inbox: Arc::new(Mutex::new(Some(inbox))),
            ..Self::default()
        }
    }

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
        if let Some(sender) = &self.sender {
            let _ = sender.send(derivations);
            self.wake.notify_one();
        }
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
    pub fn hold(&self, evaluation: EvaluationId) -> Hold {
        *self.held().entry(evaluation).or_default() += 1;
        Hold {
            held: self.clone(),
            evaluation,
        }
    }

    pub fn holds(&self, evaluation: EvaluationId) -> bool {
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

#[derive(Clone, Debug)]
pub struct DbContext {
    pub worker_db: WorkerDb,
    pub web_db: WebDb,
    pub config: Arc<RuntimeConfig>,
    pub storage: StorageCtx,
    pub shutdown: Shutdown,
    pub events: gradient_types::EventBus,
    pub delivery_wake: Arc<Notify>,
    pub probe_requests: ProbeRequests,
    pub startable_set: StartableSet,
    pub held_evaluations: HeldEvaluations,
}

impl DbContext {
    pub fn in_transaction(&self, tx: Arc<sea_orm::DatabaseTransaction>) -> DbContext {
        DbContext {
            worker_db: self.worker_db.in_transaction(tx),
            startable_set: self.startable_set.staged(),
            probe_requests: self.probe_requests.staged(),
            ..self.clone()
        }
    }

    pub fn detached(&self) -> DbContext {
        DbContext {
            worker_db: self.worker_db.detached(),
            startable_set: self.startable_set.unstaged(),
            probe_requests: self.probe_requests.unstaged(),
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_evaluation_stays_held_until_its_last_hold_drops() {
        let held = HeldEvaluations::default();
        let evaluation = EvaluationId::now_v7();

        let first = held.hold(evaluation);
        let second = held.hold(evaluation);
        drop(first);
        assert!(held.holds(evaluation));

        drop(second);
        assert!(!held.holds(evaluation));
    }

    #[tokio::test]
    async fn a_probe_request_inside_a_transaction_waits_for_the_commit() {
        let requests = ProbeRequests::channel();
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
        let requests = ProbeRequests::channel();
        let mut inbox = requests.take_inbox().expect("a fresh channel has one");

        requests.staged().send(vec![DerivationId::now_v7()]);
        requests.staged().publish();

        assert!(inbox.try_recv().is_err());
    }
}
