/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 *
 * Security probe: connect to a Gradient server's `/proto` endpoint with a
 * never-before-seen worker UUID and zero auth tokens.
 *
 * Per docs/src/contributors/proto/connection.md (`403` `unknown worker`) the
 * expected outcome is `Reject`. If the server instead returns `InitAck`, an
 * unauthenticated client has been admitted in "open mode" (`PeerAuth::Open`,
 * backend/gradient-pool/src/peer_auth.rs).
 *
 * Usage: cargo run -p gradient-wire --example probe_handshake -- ws://127.0.0.1:3000/proto
 *
 * Exit codes:
 *   0 - server rejected the connection (documented/secure behaviour)
 *   2 - server returned InitAck (open-mode auth bypass confirmed)
 *   1 - protocol/transport error
 */

use gradient_wire::client::dial;
use gradient_wire::messages::{ClientMessage, GradientCapabilities, ServerMessage};
use gradient_wire::session::frame::ProtoSocket;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:3000/proto".to_string());

    let worker_id = Uuid::now_v7().to_string();
    eprintln!("[probe] connecting to {url} as fresh worker_id={worker_id}");

    let mut ws = match dial(&url).await {
        Ok(socket) => socket,
        Err(e) => {
            eprintln!("[probe] WebSocket connect failed: {e}");
            std::process::exit(1);
        }
    };

    let init = ClientMessage::InitConnection {
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

    send(&mut ws, &ClientMessage::AuthResponse { tokens: vec![] }).await;

    match recv(&mut ws).await {
        ServerMessage::InitAck {
            capabilities,
            authorized_peers,
            failed_peers,
        } => {
            let version = ws.version().unwrap_or_default();
            eprintln!(
                "[probe] !!! InitAck received: version={version} \
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

async fn send(ws: &mut ProtoSocket, msg: &ClientMessage) {
    ws.send_client_msg(msg).await.expect("ws send");
}

async fn recv(ws: &mut ProtoSocket) -> ServerMessage {
    let Some(msg) = ws.recv_server_msg().await else {
        eprintln!("[probe] connection closed by server");
        std::process::exit(1);
    };

    msg
}
