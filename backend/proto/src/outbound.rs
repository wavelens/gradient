/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background loop that connects outbound to workers with registered URLs.
//!
//! When a worker registration has a non-null `url`, the server periodically
//! attempts to connect to that URL via WebSocket.  Once connected the same
//! [`handle_socket`](crate::handler::handle_socket) function drives the
//! connection — the protocol is identical regardless of who initiated the
//! transport.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tracing::{debug, error, info, warn};

use entity::worker_registration::{Column, Entity as EWorkerRegistration};

use crate::handler::{ProtoSocket, handle_socket};
use crate::scheduler::Scheduler;

/// Spawn the outbound connection loop as a detached tokio task.
pub fn start_outbound_loop(scheduler: Arc<Scheduler>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        let connecting: Arc<Mutex<HashSet<String>>> = Arc::default();
        info!("outbound worker connection loop started");
        loop {
            interval.tick().await;
            connect_to_registered_workers(&scheduler, &connecting).await;
        }
    });
}

async fn connect_to_registered_workers(
    scheduler: &Arc<Scheduler>,
    connecting: &Arc<Mutex<HashSet<String>>>,
) {
    let state = &scheduler.state;

    // Find all worker registrations that have a URL set.
    let registrations = match EWorkerRegistration::find()
        .filter(Column::Url.is_not_null())
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "failed to query worker registrations for outbound connections");
            return;
        }
    };

    // Deduplicate by worker_id — multiple orgs can register the same worker.
    let mut seen = HashSet::new();
    for reg in registrations {
        let Some(url) = reg.url.as_deref() else { continue };
        if url.is_empty() || !seen.insert(reg.worker_id.clone()) {
            continue;
        }

        // Skip workers already connected (inbound or outbound).
        if scheduler.is_worker_connected(&reg.worker_id).await {
            continue;
        }

        // Skip workers with a connection attempt already in progress.
        {
            let mut guard = connecting.lock().await;
            if guard.contains(&reg.worker_id) {
                continue;
            }
            guard.insert(reg.worker_id.clone());
        }

        let url = url.to_owned();
        let worker_id = reg.worker_id.clone();
        let scheduler = Arc::clone(scheduler);
        let connecting = Arc::clone(connecting);

        tokio::spawn(async move {
            debug!(%worker_id, %url, "connecting outbound to worker");

            let result = tokio::time::timeout(
                Duration::from_secs(10),
                connect_async(&url),
            )
            .await;

            match result {
                Ok(Ok((stream, _response))) => {
                    info!(%worker_id, %url, "outbound connection established");
                    let socket = ProtoSocket::Tungstenite(stream);
                    handle_socket(socket, Arc::clone(&scheduler.state), Arc::clone(&scheduler), true).await;
                    info!(%worker_id, "outbound connection closed");
                }
                Ok(Err(e)) => {
                    error!(%worker_id, %url, error = %e, "outbound connection failed");
                }
                Err(_) => {
                    error!(%worker_id, %url, "outbound connection timed out (10s)");
                }
            }

            connecting.lock().await.remove(&worker_id);
        });
    }
}
