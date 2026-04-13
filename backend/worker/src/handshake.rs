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

use crate::config::WorkerConfig;
use crate::connection::ProtoConnection;

/// Result of a successful handshake.
#[derive(Debug)]
pub struct HandshakeResult {
    /// Capabilities negotiated with the server (AND of client offer + server accept).
    pub negotiated: GradientCapabilities,
    /// Protocol version the server reported in `InitAck`.
    pub server_version: u16,
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
    // Wildcard entries (`*:token`) are expanded here.
    let tokens = WorkerConfig::resolve_tokens_for_challenge(&peer_tokens, &challenge_peers);

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
                client_version = PROTO_VERSION,
                authorized = authorized_peers.len(),
                failed = failed_peers.len(),
                "handshake successful"
            );
            if !failed_peers.is_empty() {
                for fp in &failed_peers {
                    tracing::warn!(peer_id = %fp.peer_id, reason = %fp.reason, "peer auth failed");
                }
            }
            Ok(HandshakeResult { negotiated, server_version: version })
        }
        ServerMessage::Reject { code, reason } => {
            bail!("server rejected connection (code {code}): {reason}");
        }
        other => bail!("unexpected message during handshake: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::prelude::{MockProtoServer, MockServerConn};

    fn all_caps() -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: false,
            fetch: true,
            eval: true,
            build: true,
            sign: true,
            cache: false,
        }
    }

    fn no_caps() -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: false,
            fetch: false,
            eval: false,
            build: false,
            sign: false,
            cache: false,
        }
    }

    async fn run_server(mut sc: MockServerConn, challenge_peers: Vec<String>, response: ServerMessage) {
        // Receive InitConnection.
        let _ = sc.recv().await.unwrap();
        // Send AuthChallenge.
        sc.send(ServerMessage::AuthChallenge { peers: challenge_peers })
            .await
            .unwrap();
        // Receive AuthResponse.
        let _ = sc.recv().await.unwrap();
        // Send final response.
        sc.send(response).await.unwrap();
    }

    #[tokio::test]
    async fn handshake_success() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let ack = ServerMessage::InitAck {
            version: PROTO_VERSION,
            capabilities: all_caps(),
            authorized_peers: vec!["peer-1".to_owned()],
            failed_peers: vec![],
        };

        let server_task = tokio::spawn(async move {
            let sc = server.accept().await;
            run_server(sc, vec!["peer-1".to_owned()], ack).await;
        });

        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        let result = perform_handshake(
            &mut conn,
            "worker-id".to_owned(),
            vec![("peer-1".to_owned(), "tok".to_owned())],
            all_caps(),
        )
        .await
        .unwrap();

        assert_eq!(result.server_version, PROTO_VERSION);
        assert!(result.negotiated.eval);
        assert!(result.negotiated.build);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_reject_at_challenge() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let _ = sc.recv().await.unwrap(); // InitConnection
            sc.send(ServerMessage::Reject {
                code: 403,
                reason: "banned".to_owned(),
            })
            .await
            .unwrap();
        });

        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        let err = perform_handshake(&mut conn, "wid".to_owned(), vec![], no_caps())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("banned"), "unexpected error: {err}");
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_reject_at_ack() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let _ = sc.recv().await.unwrap(); // InitConnection
            sc.send(ServerMessage::AuthChallenge { peers: vec![] })
                .await
                .unwrap();
            let _ = sc.recv().await.unwrap(); // AuthResponse
            sc.send(ServerMessage::Reject {
                code: 401,
                reason: "bad token".to_owned(),
            })
            .await
            .unwrap();
        });

        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        let err = perform_handshake(&mut conn, "wid".to_owned(), vec![], no_caps())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("bad token"), "unexpected error: {err}");
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_unexpected_message_at_challenge() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let _ = sc.recv().await.unwrap(); // InitConnection
            // Send Draining instead of AuthChallenge.
            sc.send(ServerMessage::Draining).await.unwrap();
        });

        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        let err = perform_handshake(&mut conn, "wid".to_owned(), vec![], no_caps())
            .await
            .unwrap_err();

        assert!(
            err.to_string().to_lowercase().contains("authchallenge"),
            "unexpected error: {err}"
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_wildcard_expansion() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        // Server-side: capture the AuthResponse and check it.
        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let _ = sc.recv().await.unwrap(); // InitConnection

            sc.send(ServerMessage::AuthChallenge {
                peers: vec!["p1".to_owned(), "p2".to_owned()],
            })
            .await
            .unwrap();

            let auth_resp = sc.recv().await.unwrap();
            if let ClientMessage::AuthResponse { tokens } = auth_resp {
                let map: std::collections::HashMap<_, _> = tokens.into_iter().collect();
                assert_eq!(map["p1"], "wild-tok");
                assert_eq!(map["p2"], "wild-tok");
            } else {
                panic!("expected AuthResponse");
            }

            sc.send(ServerMessage::InitAck {
                version: PROTO_VERSION,
                capabilities: no_caps(),
                authorized_peers: vec![],
                failed_peers: vec![],
            })
            .await
            .unwrap();
        });

        let peer_tokens = vec![("*".to_owned(), "wild-tok".to_owned())];
        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        let _ = perform_handshake(&mut conn, "wid".to_owned(), peer_tokens, no_caps())
            .await
            .unwrap();

        server_task.await.unwrap();
    }
}
