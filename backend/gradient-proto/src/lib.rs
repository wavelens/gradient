/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Gradient protocol crate.
//!
//! Defines the wire types, framing, and handshake state machine shared
//! across gradient-server (web crate) and gradient-worker.
//!
//! Layout:
//! - `messages` - wire message types, rkyv codecs, and shared wire constants.
//! - `session::{frame, handshake}` - direction-generic transport framing and
//!   the two role-symmetric handshake drivers (`as_peer`, `as_authority`).
//!   Both operate on an established `ProtoSocket`, regardless of which side
//!   dialed/accepted the underlying TCP+WS connection.
//! - `client::dial` - pure outbound TCP+WS dial.
//! - `server::{accept_axum, accept_tungstenite}` - inbound accept adapters.
//! - `traits` - `PeerIdentity`, `CapabilitiesProvider`, `PeerAuthority`,
//!   plus the worker-side `WorkerStore`, `DrvReader`, `JobReporter`.
//! - `handler` - gradient-server's session loop, dispatch table, and the
//!   state-coupled NAR/cache/eval-cache handlers.
//! - `outbound` - server's dial loop to discoverable workers.
//!
//! Every connection composes `client::dial` or a `server::accept_*` adapter
//! (producing a `ProtoSocket`) with the handshake role it plays:
//! - worker dials server: `dial` + `as_peer`
//! - server dials worker (discoverable): outbound loop + `as_authority`
//! - server accepts worker (axum): `accept_axum` + `as_authority`
//! - worker accepts server (tungstenite): `accept_tungstenite` + `as_peer`

pub mod client;
pub mod handler;
pub mod ingest;
pub mod messages;
pub mod outbound;
pub mod server;
pub mod session;
pub mod traits;

#[cfg(test)]
mod tests;

pub use handler::{ProtoLimiter, proto_router};
pub use messages::{ClientMessage, PROTO_VERSION, ServerMessage};

pub use gradient_scheduler::Scheduler;
pub use gradient_scheduler::WorkerInfo;
