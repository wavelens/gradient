/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Accept-side adapters: wrap an already-upgraded WebSocket into the unified
//! `ProtoSocket`, then pick the handshake role from
//! `crate::session::handshake::{as_peer, as_authority}`.

pub use crate::session::frame::accept_tungstenite;

/// Wrap an axum-upgraded WebSocket into the unified `ProtoSocket`.
pub fn accept_axum(ws: axum::extract::ws::WebSocket) -> crate::session::frame::ProtoSocket {
    crate::session::frame::ProtoSocket::Axum(Box::new(ws))
}
