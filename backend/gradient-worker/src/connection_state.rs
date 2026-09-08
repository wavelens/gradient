/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Phantom-type states for the worker's connection lifecycle.
//!
//! [`Worker<Connected>`] holds an active [`ProtoConnection`] and can enter the
//! dispatch loop via [`Worker::run`].
//!
//! [`Worker<Disconnected>`] has no connection - it can reconnect via
//! [`Worker::reconnect`], which produces a fresh [`Worker<Connected>`].
//!
//! The state is encoded in the type parameter so the compiler prevents calling
//! `run()` on an already-running worker or `reconnect()` on a connected one.

use crate::connection::ProtoConnection;

// ── State types ───────────────────────────────────────────────────────────────

/// The worker has an active connection and can enter `run()`.
pub struct Connected {
    pub(crate) conn: ProtoConnection,
}

/// The worker has no connection and can call `reconnect()`.
pub struct Disconnected;

// ── RunOutcome ────────────────────────────────────────────────────────────────

/// Why the dispatch loop (`Worker::run`) exited. Every variant reconnects; the
/// worker process only ends on a local shutdown signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Server closed the connection cleanly - reconnecting is appropriate.
    CleanDisconnect,
    /// Server sent `Draining` - it is going away for a deploy or maintenance.
    /// In-flight work is finished, then the worker reconnects until it returns.
    Drained,
    /// Server refused the session (post-handshake `Reject`), so nothing was
    /// served. Reconnecting is appropriate but must back off.
    Refused,
}
