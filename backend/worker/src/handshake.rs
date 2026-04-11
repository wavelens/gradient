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

/// Perform the `InitConnection` / `InitAck` handshake.
///
/// `peer_id` — persistent UUID loaded from disk (or freshly generated on first start).
/// `token`   — API key, read from `--token-file` if provided.
pub async fn perform_handshake(
    conn: &mut ProtoConnection,
    peer_id: String,
    token: Option<String>,
    capabilities: GradientCapabilities,
) -> Result<HandshakeResult> {
    debug!("sending InitConnection");
    conn.send(ClientMessage::InitConnection {
        version: PROTO_VERSION,
        capabilities,
        id: peer_id,
        token,
    })
    .await
    .context("failed to send InitConnection")?;

    let response = conn
        .recv()
        .await
        .context("connection closed before InitAck")?
        .context("server closed the connection without responding")?;

    match response {
        ServerMessage::InitAck { version, capabilities: negotiated } => {
            info!(server_version = version, "handshake successful");
            Ok(HandshakeResult { negotiated })
        }
        ServerMessage::Reject { code, reason } => {
            bail!("server rejected connection (code {code}): {reason}");
        }
        other => {
            bail!("unexpected message during handshake: {other:?}");
        }
    }
}
