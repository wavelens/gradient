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
}

impl ProbeRequests {
    pub fn channel() -> Self {
        let (sender, inbox) = mpsc::unbounded_channel();
        Self {
            sender: Some(sender),
            inbox: Arc::new(Mutex::new(Some(inbox))),
        }
    }

    /// The probe loop is not optional.
    /// Need is stopping at shared builds the probe has not answered.
    /// Nothing below an entry point is promoted without the loop.
    pub fn send(&self, derivations: Vec<DerivationId>) {
        if derivations.is_empty() {
            return;
        }

        if let Some(sender) = &self.sender {
            let _ = sender.send(derivations);
        }
    }

    pub fn take_inbox(&self) -> Option<mpsc::UnboundedReceiver<Vec<DerivationId>>> {
        self.inbox.lock().ok()?.take()
    }
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
            ..self.clone()
        }
    }

    pub fn detached(&self) -> DbContext {
        DbContext {
            worker_db: self.worker_db.detached(),
            startable_set: self.startable_set.unstaged(),
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
}
