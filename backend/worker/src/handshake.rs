/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Protocol handshake: `InitConnection` → `InitAck` / `Reject`.
//!
//! After a successful handshake the server has validated our token and
//! negotiated capabilities.  The negotiated [`GradientCapabilities`] may be a
//! strict subset of what we advertised — the server may disable capabilities
//! it is not configured to accept.

use anyhow::{Context, Result, bail};
use proto::messages::{ClientMessage, GradientCapabilities, ServerMessage, PROTO_VERSION};
use tracing::{debug, info};

use crate::connection::ProtoConnection;

/// Result of a successful handshake.
pub struct HandshakeResult {
    /// Capabilities negotiated with the server (AND of client offer + server accept).
    pub negotiated: GradientCapabilities,
}

/// Perform the full challenge-response handshake.
///
/// `peer_id`     — persistent UUID loaded from disk (or freshly generated on first start).
/// `peer_tokens` — `(peer_id, plaintext_token)` pairs from `GRADIENT_WORKER_PEERS`.
/// `capabilities`— advertised capabilities.
pub async fn perform_handshake(
    conn: &mut ProtoConnection,
    peer_id: String,
    peer_tokens: Vec<(String, String)>,
    capabilities: GradientCapabilities,
) -> Result<HandshakeResult> {
    debug!("sending InitConnection");
    conn.send(ClientMessage::InitConnection {
        version: PROTO_VERSION,
        capabilities,
        id: peer_id,
    })
    .await
    .context("failed to send InitConnection")?;

    // Expect AuthChallenge.
    let challenge = conn
        .recv()
        .await
        .context("connection closed before AuthChallenge")?
        .context("server closed the connection without responding")?;

    let challenge_peers = match challenge {
        ServerMessage::AuthChallenge { peers } => peers,
        ServerMessage::Reject { code, reason } => {
            bail!("server rejected connection (code {code}): {reason}");
        }
        other => bail!("expected AuthChallenge, got: {other:?}"),
    };

    debug!(peers = ?challenge_peers, "received AuthChallenge");

    // Reply with tokens for the peers the server challenged us about.
    let tokens: Vec<(String, String)> = peer_tokens
        .into_iter()
        .filter(|(pid, _)| challenge_peers.contains(pid))
        .collect();

    conn.send(ClientMessage::AuthResponse { tokens })
        .await
        .context("failed to send AuthResponse")?;

    // Expect InitAck.
    let ack = conn
        .recv()
        .await
        .context("connection closed before InitAck")?
        .context("server closed the connection without responding")?;

    match ack {
        ServerMessage::InitAck { version, capabilities: negotiated, authorized_peers, failed_peers } => {
            info!(
                server_version = version,
                authorized = authorized_peers.len(),
                failed = failed_peers.len(),
                "handshake successful"
            );
            if !failed_peers.is_empty() {
                for fp in &failed_peers {
                    tracing::warn!(peer_id = %fp.peer_id, reason = %fp.reason, "peer auth failed");
                }
            }
            Ok(HandshakeResult { negotiated })
        }
        ServerMessage::Reject { code, reason } => {
            bail!("server rejected connection (code {code}): {reason}");
        }
        other => bail!("unexpected message during handshake: {other:?}"),
    }
}
