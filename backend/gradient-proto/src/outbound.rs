/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gradient_util::supervision::ChildSpec;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use gradient_entity::worker_registration::{Column, Entity as EWorkerRegistration};

use crate::handler::{SessionsHandle, handle_socket};
use gradient_scheduler::Scheduler;

pub fn start_outbound_loop(scheduler: Arc<Scheduler>, sessions: Arc<SessionsHandle>) {
    let connecting: Arc<Mutex<HashSet<String>>> = Arc::default();
    let pass_scheduler = Arc::clone(&scheduler);
    scheduler.state.shutdown.supervise(ChildSpec::periodic(
        "outbound-connect",
        Duration::from_secs(15),
        Duration::from_secs(60),
        move || {
            let scheduler = Arc::clone(&pass_scheduler);
            let sessions = Arc::clone(&sessions);
            let connecting = Arc::clone(&connecting);
            async move {
                connect_to_registered_workers(&scheduler, &sessions, &connecting).await;
                Ok(())
            }
        },
    ));
}

async fn connect_to_registered_workers(
    scheduler: &Arc<Scheduler>,
    sessions: &Arc<SessionsHandle>,
    connecting: &Arc<Mutex<HashSet<String>>>,
) {
    let state = &scheduler.state;

    let registrations = match EWorkerRegistration::find()
        .filter(Column::Url.is_not_null())
        .all(&state.worker_db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "failed to query worker registrations for outbound connections");
            return;
        }
    };

    let mut seen = HashSet::new();
    for reg in registrations {
        let Some(url) = reg.url.as_deref() else {
            continue;
        };
        if url.is_empty() || !seen.insert(reg.worker_id.clone()) {
            continue;
        }

        if scheduler.is_worker_connected(&reg.worker_id).await {
            continue;
        }

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
        let sessions = Arc::clone(sessions);
        let connecting = Arc::clone(connecting);
        let shutdown = scheduler.state.shutdown.clone();

        shutdown.spawn(async move {
            debug!(%worker_id, %url, "connecting outbound to worker");

            let result =
                tokio::time::timeout(Duration::from_secs(10), gradient_wire::client::dial(&url))
                    .await;

            match result {
                Ok(Ok(socket)) => {
                    info!(%worker_id, %url, "outbound connection established");
                    handle_socket(
                        socket,
                        Arc::clone(&scheduler.state),
                        Arc::clone(&scheduler),
                        Arc::clone(&sessions),
                        true,
                    )
                    .await;
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
