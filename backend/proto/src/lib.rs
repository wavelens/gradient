/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Gradient protocol crate.
//!
//! Defines the wire types, framing, handshake state machine, and
//! per-capability traits shared across gradient-server (web crate),
//! gradient-worker, and gradient-proxy (closed source).
//!
//! Layout:
//! - `messages` — wire message types and rkyv codecs.
//! - `session::{frame, handshake}` — pure transport framing and the two
//!   role-symmetric handshake drivers (`as_peer`, `as_authority`). Both
//!   drivers operate on an established `ProtoSocket`, regardless of which
//!   side dialed/accepted the underlying TCP+WS connection.
//! - `client::dial` — pure outbound TCP+WS dial.
//! - `server::{accept_axum, accept_tungstenite, dispatch}` — inbound
//!   accept-side adapters and the per-message dispatch loop.
//! - `cap::{build,eval,fetch,cache,federate}` — per-capability trait pairs.
//! - `traits` — `PeerIdentity`, `CapabilitiesProvider`, `PeerAuthority`,
//!   `SessionFactory`, plus the worker-side `WorkerStore`, `DrvReader`,
//!   `JobReporter`.
//! - `handler` — gradient-server's existing inbound axum router and the
//!   state-coupled NAR serving / credential delivery helpers. Will shrink
//!   in follow-up refactors as the worker and gradient-server migrate to
//!   the new primitives.
//! - `outbound` — server's outbound-side message loop.
//!
//! Bidirectional connectivity: callers compose `client::dial` or one of the
//! `server::accept_*` adapters (producing a `ProtoSocket`) with whichever
//! handshake role they play. All four combinations are first-class:
//! - worker→server dial: `dial` + `as_peer`
//! - server→worker dial (discoverable): `dial` + `as_authority`
//! - server-accepts-worker (axum): `accept_axum` + `as_authority`
//! - worker-accepts-server (tungstenite): `accept_tungstenite` + `as_peer`

pub mod cap;
pub mod client;
pub mod handler;
pub mod messages;
pub mod outbound;
pub mod server;
pub mod session;
pub mod traits;

#[cfg(test)]
mod tests;

pub use handler::{ProtoLimiter, proto_router};
pub use messages::{ClientMessage, PROTO_VERSION, ServerMessage};

pub use scheduler::Scheduler;
pub use scheduler::WorkerInfo;
