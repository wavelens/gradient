/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod auth;
mod cache;
mod cache_consumer;
mod cache_session;
mod dialed;
mod eval_cache;
mod failures;
mod handles;
mod inbound;
mod job_events;
mod log_lane;
mod nar;
mod nar_serve;
mod refusal;
mod session;
mod session_actor;
mod sessions;
mod socket;
mod store_paths;
mod tap;
mod upload;

use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_wire::ProtoLimiter;
use gradient_wire::session::frame::{BULK_CHUNK_SIZE, MAX_PROTO_MESSAGE_SIZE};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use cache_session::handle_cache_socket;
pub use handles::SessionHandles;
pub(crate) use session::{SessionOrigin, handle_socket};
pub use sessions::SessionsHandle;

const RETRY_AFTER: HeaderValue = HeaderValue::from_static("10");

pub fn proto_router() -> OpenApiRouter<Arc<ServerState>> {
    OpenApiRouter::new().routes(routes!(ws_upgrade))
}

#[utoipa::path(
    get,
    path = "/proto",
    operation_id = "protoWebSocket",
    tag = "general",
    summary = "Binary WebSocket protocol endpoint",
    description = "Upgrades the connection to a WebSocket carrying binary frames encoded\nby the `Proto` derive of `gradient-wire`. The upgrade always succeeds; a\nserver with `GRADIENT_PROTO_DISCOVERABLE=false` then rejects the\nhandshake of a worker it did not dial itself with a `403` reject frame.\n\n**Protocol flow**\n1. Both sides send an 8-byte version frame (`GRAD`, oldest and newest\n   supported version as `u16` little-endian) and encode every later\n   frame for the highest shared version. Without one, the WebSocket\n   closes with code `1002` and the reason.\n2. Client sends `ClientMessage::InitConnection { capabilities, id }`.\n3. Server responds with `ServerMessage::AuthChallenge`, then\n   `ServerMessage::InitAck { capabilities, .. }` with the negotiated\n   capabilities.",
    responses(
        (status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"),
        (status = SERVICE_UNAVAILABLE, description = "Connection limit (`GRADIENT_PROTO_MAX_CONNECTIONS`) reached; retry after `Retry-After`")
    )
)]
async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Extension(limiter): Extension<Arc<ProtoLimiter>>,
    Extension(sessions): Extension<Arc<SessionsHandle>>,
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

    let shutdown = state.shutdown.clone();
    let handles = SessionHandles::from_state(&state);
    ws.max_message_size(MAX_PROTO_MESSAGE_SIZE)
        .max_frame_size(MAX_PROTO_MESSAGE_SIZE)
        .read_buffer_size(BULK_CHUNK_SIZE)
        .on_upgrade(move |sock| async move {
            let _ = shutdown
                .spawn(async move {
                    let _permit = permit;
                    session::handle_socket(
                        socket::ProtoSocket::axum(sock),
                        handles,
                        scheduler,
                        sessions,
                        session::SessionOrigin::WorkerDialed,
                    )
                    .await;
                })
                .await;
        })
}
