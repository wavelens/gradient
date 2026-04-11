/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use gradient_core::types::ServerState;
use rkyv::rancor::Error as RkyvError;
use std::sync::Arc;
use tracing::{debug, info, instrument, warn};

use crate::messages::{ClientMessage, GradientCapabilities, PROTO_VERSION, ServerMessage};

/// Returns the axum [`Router`] that serves the `/proto` WebSocket endpoint.
///
/// Merge this into the main router in `web`:
/// ```ignore
/// app.merge(protocol::proto_router())
/// ```
pub fn proto_router() -> Router<Arc<ServerState>> {
    Router::new().route("/proto", get(ws_upgrade))
}

/// HTTP GET `/proto` — upgrades the connection to a WebSocket.
async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Drives a single WebSocket connection for its lifetime.
///
/// Protocol flow:
/// 1. Client sends [`ClientMessage::InitConnection`] as a binary rkyv frame.
/// 2. Server responds with [`ServerMessage::InitAck`] (negotiated version +
///    capabilities) or [`ServerMessage::Error`] and closes.
/// 3. Further message types will be added here as the protocol evolves.
#[instrument(skip_all)]
async fn handle_socket(mut socket: WebSocket, state: Arc<ServerState>) {
    info!("WebSocket connection opened");

    if !state.cli.discoverable {
        send_error(&mut socket, 403, "server is not accepting connections".into()).await;
        return;
    }

    // ── Handshake ─────────────────────────────────────────────────────────────
    let Some(init_msg) = recv_client_msg(&mut socket).await else {
        return;
    };

    let (client_version, client_capabilities, peer_id, token) = match init_msg {
        ClientMessage::InitConnection { version, capabilities, id, token } => {
            (version, capabilities, id, token)
        }
        _ => {
            send_error(&mut socket, 400, "expected InitConnection".into()).await;
            return;
        }
    };

    debug!(client_version, ?client_capabilities, %peer_id, "InitConnection received");

    // TODO: validate token against configured API keys.
    // Cache-only connections (public caches) may omit the token.
    // if requires_auth(&client_capabilities) && !validate_token(&state, &token) {
    //     send_error(&mut socket, 401, "invalid token".into()).await;
    //     return;
    // }

    if client_version > PROTO_VERSION {
        send_error(&mut socket, 400, format!("unsupported protocol version {client_version}")).await;
        return;
    }

    // Negotiate: intersect client-requested capabilities with what the server
    // supports.  Each bool field is AND-ed — unknown fields added by a newer
    // client default to false, which is safe.
    let negotiated = negotiate_capabilities(&state, client_capabilities);

    let ack = ServerMessage::InitAck {
        version: PROTO_VERSION,
        capabilities: negotiated,
    };

    if send_server_msg(&mut socket, &ack).await.is_err() {
        return;
    }

    info!(client_version, "handshake complete");

    // ── Main message loop ─────────────────────────────────────────────────────
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(bytes))) => {
                match rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes) {
                    Ok(msg) => {
                        debug!(?msg, "received client message");
                        match msg {
                            ClientMessage::RequestJobList => {
                                // TODO: query pending jobs from state, chunk and stream JobCandidate list
                                let reply = ServerMessage::JobListChunk { candidates: vec![], is_final: true };
                                if send_server_msg(&mut socket, &reply).await.is_err() {
                                    break;
                                }
                            }
                            _ => {
                                // Future message types are dispatched here.
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to deserialize client message");
                        send_error(&mut socket, 400, "malformed message".into()).await;
                        break;
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => {
                debug!("connection closed by client");
                break;
            }
            Some(Ok(_)) => {
                // Text frames, pings, pongs — ignore.
            }
            Some(Err(e)) => {
                debug!(error = %e, "WebSocket error");
                break;
            }
        }
    }

    info!("WebSocket connection closed");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn recv_client_msg(socket: &mut WebSocket) -> Option<ClientMessage> {
    loop {
        match socket.recv().await? {
            Ok(Message::Binary(bytes)) => {
                match rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes) {
                    Ok(msg) => return Some(msg),
                    Err(e) => {
                        warn!(error = %e, "failed to deserialize client message");
                        send_error(socket, 400, "malformed message".into()).await;
                        return None;
                    }
                }
            }
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(e) => {
                debug!(error = %e, "WebSocket recv error");
                return None;
            }
        }
    }
}

async fn send_server_msg(socket: &mut WebSocket, msg: &ServerMessage) -> Result<(), ()> {
    let bytes = rkyv::to_bytes::<RkyvError>(msg).map_err(|e| {
        warn!(error = %e, "failed to serialize server message");
    })?;
    socket
        .send(Message::Binary(bytes.to_vec().into()))
        .await
        .map_err(|e| debug!(error = %e, "WebSocket send error"))
}

async fn send_error(socket: &mut WebSocket, code: u16, message: String) {
    let msg = ServerMessage::Error { code, message };
    let _ = send_server_msg(socket, &msg).await;
}

/// Negotiate capabilities: activate only the capabilities both sides support.
///
/// With a struct of bools, negotiation is just AND — if the server doesn't
/// know about a capability it's `false` on the server side, so the result is
/// `false` regardless of what the client requested.  Add new capabilities to
/// [`GradientCapabilities`] and set the server's supported value here.
fn negotiate_capabilities(state: &ServerState, client: GradientCapabilities) -> GradientCapabilities {
    // `core` and `cache` are server-determined — workers cannot claim them.
    // All other capabilities are AND-ed: the client opts in, the server confirms.
    GradientCapabilities {
        core: true,
        cache: state.cli.serve_cache,
        federate: client.federate && state.cli.federate_proto,
        fetch: client.fetch,
        eval: client.eval,
        build: client.build,
        sign: client.sign,
    }
}
