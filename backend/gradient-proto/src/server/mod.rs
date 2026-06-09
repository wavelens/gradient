/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Inbound proto-session helpers. Used by gradient-server (accepting worker
//! and federate connections), gradient-proxy (accepting backend worker
//! connections), and gradient-worker (accepting incoming server connections
//! in discoverable mode).
//!
//! - Accept-side adapters (e.g. `accept_axum`, `accept_tungstenite`) wrap an
//!   already-upgraded WebSocket into the unified `ProtoSocket`.
//! - `dispatch::run` drives the per-message loop after the handshake
//!   completes, routing inbound messages to the per-capability traits the
//!   caller has supplied.
//!
//! Pick the handshake role from `crate::session::handshake::{as_peer, as_authority}`
//! after wrapping the socket.

pub mod dispatch;

pub use crate::session::frame::accept_tungstenite;

/// Wrap an axum-upgraded WebSocket into the unified `ProtoSocket`.
///
/// Until Task 18 drops the axum dep from proto entirely, this is always
/// available. A future feature gate (`axum-ws`) will make it optional.
pub fn accept_axum(ws: axum::extract::ws::WebSocket) -> crate::session::frame::ProtoSocket {
    crate::session::frame::ProtoSocket::Axum(Box::new(ws))
}
