/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::ProjectId;
use gradient_wire::types::{ClusterAddress, ClusterPeer, ImportOutcome, InputOutcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSignal {
    Offers(u64),
    Reauth,
    Abort {
        job_id: String,
        reason: String,
    },
    /// The scheduler has already re-queued the in-flight jobs. The worker must reconnect instead of
    /// reporting into a session the pool no longer knows.
    Close {
        reason: String,
    },
    ClusterAssign {
        job_id: String,
    },
    StartCluster {
        attempt: String,
        roster: Vec<ClusterPeer>,
    },
    ClusterSignal {
        attempt: String,
        from: ClusterAddress,
        payload: Vec<u8>,
    },
    AbortCluster {
        attempt: String,
        reason: String,
    },
    ImportResult {
        job_id: String,
        request_id: String,
        outcome: ImportOutcome,
    },
    InputResult {
        job_id: String,
        request_id: String,
        outcome: InputOutcome,
    },
    InputProgress {
        job_id: String,
        request_id: String,
        bytes_done: u64,
        bytes_total: Option<u64>,
    },
    FetchInput {
        fetch_id: String,
        project: ProjectId,
        request: String,
    },
    PreWarmVms {
        count: u32,
    },
}

pub trait SessionPort: Send + Sync + 'static {
    fn signal(&self, signal: SessionSignal);
}

impl SessionPort for tokio::sync::mpsc::UnboundedSender<SessionSignal> {
    fn signal(&self, signal: SessionSignal) {
        let _ = self.send(signal);
    }
}
