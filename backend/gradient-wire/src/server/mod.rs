/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use crate::session::frame::accept_tungstenite;

pub fn accept_axum(ws: axum::extract::ws::WebSocket) -> crate::session::frame::ProtoSocket {
    crate::session::frame::ProtoSocket::Axum(Box::new(ws))
}
