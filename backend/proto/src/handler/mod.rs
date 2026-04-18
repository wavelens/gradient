/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod auth;
mod cache;
mod dispatch;
mod nar;
mod session;
mod socket;

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, State};
use axum::response::IntoResponse;
use axum::routing::get;
use gradient_core::types::*;
use scheduler::Scheduler;

pub(crate) use session::handle_socket;
pub(crate) use socket::ProtoSocket;

/// Returns the axum [`Router`] that serves the `/proto` WebSocket endpoint.
pub fn proto_router() -> Router<Arc<ServerState>> {
    Router::new().route("/proto", get(ws_upgrade))
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |sock| {
        session::handle_socket(socket::ProtoSocket::Axum(sock), state, scheduler, false)
    })
}
