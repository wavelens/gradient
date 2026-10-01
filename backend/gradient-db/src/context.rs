/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use std::sync::Mutex;

use tokio::sync::{Notify, mpsc};

use crate::pool::{WebDb, WorkerDb};
use crate::scheduling::startable_set::StartableSet;
use gradient_storage::StorageCtx;
use gradient_types::{DerivationId, RuntimeConfig};
use gradient_util::shutdown::Shutdown;

/// The shared builds a need update turned on, on their way to the upstream probe.
///
/// A channel rather than a call: probing is HTTP, and every need update is running
/// on a path that may be inside the graph writer's transaction, where a network
/// round trip would hold the one writer to the graph for its duration. The
/// receiving end rides along so the composition root can hand it to the probe loop
/// without a second field; [`Self::default`] is a sink with no receiver at all, so
/// a harness running no loop drops what it is handed.
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

    /// Hand the probe loop what just gained need. Never blocks and never fails,
    /// but it is not optional: need stops at a shared build the probe has not answered
    /// for, so a loop that never starts is a fleet that promotes nothing below an
    /// entry point. The loop sweeps for need whose request was lost; a loop that
    /// is down is the supervisor's, and reports itself through health.
    pub fn send(&self, derivations: Vec<DerivationId>) {
        if derivations.is_empty() {
            return;
        }

        if let Some(sender) = &self.sender {
            let _ = sender.send(derivations);
        }
    }

    /// The receiving end, once. A second caller gets `None`, which is what keeps
    /// two probe loops from splitting the stream between them.
    pub fn take_inbox(&self) -> Option<mpsc::UnboundedReceiver<Vec<DerivationId>>> {
        self.inbox.lock().ok()?.take()
    }
}

/// Persistence-layer slice threaded through every `db` function: the two
/// connection pools, resolved config, storage handles, the shutdown
/// coordinator and board-event broadcast used by db-side background tasks, and
/// the wake the effects actor waits on. Nothing here reaches `ci`: what a state
/// change owes the outside world is a pending delivery, not a call.
#[derive(Clone, Debug)]
pub struct DbContext {
    pub worker_db: WorkerDb,
    pub web_db: WebDb,
    pub config: Arc<RuntimeConfig>,
    pub storage: StorageCtx,
    pub shutdown: Shutdown,
    pub events: gradient_types::EventBus,
    /// Nudged after every committed write that owes an effect, so the effects
    /// actor claims the row it just wrote instead of waiting out its tick.
    pub delivery_wake: Arc<Notify>,
    /// Where a need update reports what it turned on, for the probe loop.
    pub probe_requests: ProbeRequests,
    /// Where a shared build entering or leaving `Queued` is reported, for dispatch.
    pub startable_set: StartableSet,
}

impl DbContext {
    /// The same context with every statement bound to `tx`. Its startable-set
    /// moves are staged until the owner of `tx` publishes them after the commit.
    pub fn in_transaction(&self, tx: Arc<sea_orm::DatabaseTransaction>) -> DbContext {
        DbContext {
            worker_db: self.worker_db.in_transaction(tx),
            startable_set: self.startable_set.staged(),
            ..self.clone()
        }
    }

    /// The same context on the pool, for work spawned past the current transaction.
    pub fn detached(&self) -> DbContext {
        DbContext {
            worker_db: self.worker_db.detached(),
            startable_set: self.startable_set.unstaged(),
            ..self.clone()
        }
    }
}
