/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// What the scheduler pushes to a session. Every variant is idempotent;
/// `Offers` carries the generation that lets a session coalesce a burst.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSignal {
    Offers(u64),
    Reauth,
    Abort {
        job_id: String,
        reason: String,
    },
    Drain,
    /// Tear the session down now. Unlike [`SessionSignal::Drain`] this does not
    /// wait for in-flight jobs: the scheduler has already re-queued them, so the
    /// worker must drop the connection and reconnect rather than keep reporting
    /// into a session the pool no longer knows about.
    Close {
        reason: String,
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
