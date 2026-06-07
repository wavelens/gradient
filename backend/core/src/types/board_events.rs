/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Live events broadcast to WebSocket subscribers. Held on [`ServerState`] so
//! both the scheduler (queue/worker events) and the DB status helpers
//! (evaluation/build/cache changes) can publish without a cyclic crate
//! dependency. The web layer filters the stream per subscribed resource.

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
    /// An evaluation changed status. `project` lets the project channel filter
    /// without a lookup; the evaluation channel filters on `evaluation_id`.
    EvaluationStatusChanged {
        project: Option<Uuid>,
        evaluation_id: Uuid,
        status: i16,
    },
    /// A build changed status. Filtered by `evaluation_id`; the project channel
    /// resolves the owning evaluation from the evaluations it has seen.
    BuildStatusChanged {
        evaluation_id: Uuid,
        build_id: Uuid,
        status: i16,
    },
    /// Cache contents or stats changed (build cached, NAR deleted, GC). A
    /// content-free ping: subscribers refetch their own scope-filtered view.
    CacheChanged,
}
