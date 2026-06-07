/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Live events broadcast to Job Board WebSocket subscribers. The web layer
//! masks per-org events to the caller's scope before forwarding.

use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BoardEvent {
    JobDispatched {
        organization: Uuid,
        worker_id: String,
        kind: i16,
        score: f64,
        build_id: Option<Uuid>,
        evaluation_id: Uuid,
    },
    WorkerConnected {
        organization: Uuid,
        worker_id: String,
    },
    WorkerDisconnected {
        worker_id: String,
    },
    QueueDepth {
        workers: usize,
        pending: usize,
        active: usize,
    },
}
