/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod auth;
mod cache;
mod cache_consumer;
mod cache_session;
mod dispatch;
mod limiter;
mod nar;
mod session;
mod socket;

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use gradient_core::types::*;
use scheduler::Scheduler;

pub use cache_session::handle_cache_socket;
pub use crate::session::frame::MAX_PROTO_MESSAGE_SIZE;
pub use limiter::{PerIpLimiter, ProtoLimiter};
pub(crate) use session::handle_socket;
pub(crate) use socket::ProtoSocket;

#[cfg(test)]
pub(crate) use socket::{HANDSHAKE_TIMEOUT, NAR_PUSH_CHUNK_SIZE};

/// `Retry-After` value returned with a 503 when the proto connection cap is
/// hit - long enough to absorb a brief surge, short enough that a recovered
/// worker reconnects promptly.
const RETRY_AFTER: HeaderValue = HeaderValue::from_static("10");

/// Returns the axum [`Router`] that serves the `/proto` WebSocket endpoint.
pub fn proto_router() -> Router<Arc<ServerState>> {
    Router::new().route("/proto", get(ws_upgrade))
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Extension(limiter): Extension<Arc<ProtoLimiter>>,
) -> Response {
    let Some(permit) = limiter.try_acquire() else {
        tracing::warn!(
            capacity = limiter.capacity(),
            in_use = limiter.in_use(),
            "proto connection limit reached; rejecting upgrade",
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, RETRY_AFTER)],
            "proto connection limit reached",
        )
            .into_response();
    };

    ws.max_message_size(MAX_PROTO_MESSAGE_SIZE)
        .max_frame_size(MAX_PROTO_MESSAGE_SIZE)
        .on_upgrade(move |sock| async move {
            let _permit = permit;
            session::handle_socket(
                socket::ProtoSocket::Axum(Box::new(sock)),
                state,
                scheduler,
                false,
            )
            .await;
        })
}
