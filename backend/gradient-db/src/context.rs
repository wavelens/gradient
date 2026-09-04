/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use tokio::sync::broadcast;

use super::pool::{WebDb, WorkerDb};
use super::status_reactor::StatusReactor;
use gradient_storage::StorageCtx;
use gradient_types::{BoardEvent, RuntimeConfig};
use gradient_util::shutdown::Shutdown;

/// Persistence-layer slice threaded through every `db` function: the two
/// connection pools, resolved config, storage handles, the shutdown
/// coordinator and board-event broadcast used by db-side background tasks, and
/// the terminal-status reaction hook ([`StatusReactor`]) that inverts the old
/// `db -> ci` edge.
#[derive(Clone, Debug)]
pub struct DbContext {
    pub worker_db: WorkerDb,
    pub web_db: WebDb,
    pub config: Arc<RuntimeConfig>,
    pub storage: StorageCtx,
    pub shutdown: Shutdown,
    pub board_events: broadcast::Sender<BoardEvent>,
    pub reactor: Arc<dyn StatusReactor>,
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
