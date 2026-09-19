/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use std::sync::Mutex;

use tokio::sync::{Notify, broadcast, mpsc};

use super::pool::{WebDb, WorkerDb};
use gradient_storage::StorageCtx;
use gradient_types::{BoardEvent, DerivationId, RuntimeConfig};
use gradient_util::shutdown::Shutdown;

/// The anchors a demand recompute turned on, on their way to the upstream probe.
///
/// A channel rather than a call: probing is HTTP, and every demand recompute runs
/// on a path that may be inside the graph actor's transaction, where a network
/// round trip would hold the one writer to the graph for its duration. The
/// receiving end rides along so the composition root can hand it to the probe loop
/// without a second field; [`Self::default`] is a sink with no receiver at all, so
/// a harness that runs no loop drops what it is handed.
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

    /// Hand the probe loop what just gained demand. Never blocks and never fails,
    /// but it is not optional: demand stops at an anchor the probe has not answered
    /// for, so a loop that never runs is a fleet that promotes nothing below an
    /// entry point. The loop sweeps for demand whose request was lost; a loop that
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
/// change owes the outside world is an `outbox` row, not a call.
#[derive(Clone, Debug)]
pub struct DbContext {
    pub worker_db: WorkerDb,
    pub web_db: WebDb,
    pub config: Arc<RuntimeConfig>,
    pub storage: StorageCtx,
    pub shutdown: Shutdown,
    pub board_events: broadcast::Sender<BoardEvent>,
    /// Nudged after every committed write that owes an effect, so the effects
    /// actor claims the row it just wrote instead of waiting out its tick.
    pub outbox_wake: Arc<Notify>,
    /// Where a demand recompute reports what it turned on, for the probe loop.
    pub probe_requests: ProbeRequests,
}

impl DbContext {
    /// The same context with every statement bound to `tx`.
    pub fn in_transaction(&self, tx: Arc<sea_orm::DatabaseTransaction>) -> DbContext {
        DbContext {
            worker_db: self.worker_db.in_transaction(tx),
            ..self.clone()
        }
    }

    /// The same context on the pool, for work spawned past the current transaction.
    pub fn detached(&self) -> DbContext {
        DbContext {
            worker_db: self.worker_db.detached(),
            ..self.clone()
        }
    }
}
