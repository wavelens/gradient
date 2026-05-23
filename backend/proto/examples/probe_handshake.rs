/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 *
 * Security probe: connect to a Gradient server's `/proto` endpoint with a
 * never-before-seen worker UUID and zero auth tokens.
 *
 * Per docs/src/development/proto.md ("Server rejects when no peers have
 * registered this worker ID (unknown worker)") the expected outcome is
 * `Reject`. If the server instead returns `InitAck`, an unauthenticated
 * client has been admitted in "open mode" (PeerAuth::Open) - see
 * backend/proto/src/handler/session.rs:154.
 *
 * Usage: cargo run -p proto --example probe_handshake -- ws://127.0.0.1:3000/proto
 *
 * Exit codes:
 *   0 - server rejected the connection (documented/secure behaviour)
 *   2 - server returned InitAck (open-mode auth bypass confirmed)
 *   1 - protocol/transport error
 */

use futures::{SinkExt, StreamExt};
use proto::messages::{
    ClientMessage, GradientCapabilities, PROTO_VERSION, ServerMessage, decode_server_message,
};
use rkyv::rancor::Error as RkyvError;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

#[tokio::main]
async fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:3000/proto".to_string());

    let worker_id = Uuid::now_v7().to_string();
    eprintln!("[probe] connecting to {url} as fresh worker_id={worker_id}");

    let (mut ws, _resp) = match connect_async(&url).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[probe] WebSocket connect failed: {e}");
            std::process::exit(1);
        }
    };

    // ── 1. InitConnection ────────────────────────────────────────────────────
    let init = ClientMessage::InitConnection {
        version: PROTO_VERSION,
        capabilities: GradientCapabilities {
            core: false,
            federate: false,
            fetch: true,
            eval: true,
            build: true,
            cache: false,
        },
        id: worker_id,
    };
    send(&mut ws, &init).await;

    // ── 2. Expect AuthChallenge ──────────────────────────────────────────────
    match recv(&mut ws).await {
        ServerMessage::AuthChallenge { peers } => {
            eprintln!("[probe] AuthChallenge received, peers={peers:?}");
        }
        ServerMessage::Reject { code, reason } => {
            eprintln!("[probe] REJECTED at init (code {code}): {reason}");
            eprintln!("[probe] OK - server refused unknown worker (secure)");
            std::process::exit(0);
        }
        other => {
            eprintln!("[probe] unexpected reply to InitConnection: {other:?}");
            std::process::exit(1);
        }
    }

    // ── 3. AuthResponse with no tokens ───────────────────────────────────────
    send(&mut ws, &ClientMessage::AuthResponse { tokens: vec![] }).await;

    // ── 4. InitAck or Reject? ────────────────────────────────────────────────
    match recv(&mut ws).await {
        ServerMessage::InitAck {
            version,
            capabilities,
            authorized_peers,
            failed_peers,
        } => {
            eprintln!(
                "[probe] !!! InitAck received: server_version={version} \
                 negotiated={capabilities:?} authorized_peers={authorized_peers:?} \
                 failed_peers={failed_peers:?}"
            );
            eprintln!(
                "[probe] VULNERABLE - unknown worker admitted in open mode \
                 with zero credentials"
            );
            std::process::exit(2);
        }
        ServerMessage::Reject { code, reason } => {
            eprintln!("[probe] REJECTED after auth (code {code}): {reason}");
            eprintln!("[probe] OK - server refused unknown worker (secure)");
            std::process::exit(0);
        }
        other => {
            eprintln!("[probe] unexpected reply to AuthResponse: {other:?}");
            std::process::exit(1);
        }
    }
}

async fn send(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    msg: &ClientMessage,
) {
    let bytes = rkyv::to_bytes::<RkyvError>(msg).expect("rkyv serialize");
    ws.send(Message::Binary(bytes.to_vec().into()))
        .await
        .expect("ws send");
}

async fn recv(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ServerMessage {
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(bytes))) => {
                return decode_server_message(&bytes).expect("rkyv deserialize ServerMessage");
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(other)) => {
                eprintln!("[probe] non-binary frame: {other:?}");
                std::process::exit(1);
            }
            Some(Err(e)) => {
                eprintln!("[probe] ws recv error: {e}");
                std::process::exit(1);
            }
            None => {
                eprintln!("[probe] connection closed by server");
                std::process::exit(1);
            }
        }
    }
}
