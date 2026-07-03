/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Protocol handshake: `InitConnection` → `InitAck` / `Reject`.
//!
//! The wire sequence is driven by gradient-proto's shared
//! [`as_peer`](gradient_proto::session::handshake::as_peer) FSM; this module
//! only supplies the worker's identity (persistent UUID plus the peer tokens
//! from `GRADIENT_WORKER_PEERS`, wildcards expanded per challenge) and its
//! advertised capabilities. The negotiated [`GradientCapabilities`] may be a
//! strict subset of what we advertised.

use anyhow::Result;
use async_trait::async_trait;
use gradient_proto::messages::{GradientCapabilities, PROTO_VERSION};
use gradient_proto::session::handshake::{HandshakeResult, as_peer};
use gradient_proto::traits::{CapabilitiesProvider, PeerIdentity};
use tracing::info;

use crate::config::WorkerConfig;
use crate::connection::ProtoConnection;

struct WorkerIdentity {
    peer_id: String,
    peer_tokens: Vec<(String, String)>,
}

#[async_trait]
impl PeerIdentity for WorkerIdentity {
    fn peer_id(&self) -> String {
        self.peer_id.clone()
    }

    async fn tokens_for(&self, peers: &[String]) -> Result<Vec<(String, String)>> {
        Ok(WorkerConfig::resolve_tokens_for_challenge(
            &self.peer_tokens,
            peers,
        ))
    }
}

struct StaticCapabilities(GradientCapabilities);

#[async_trait]
impl CapabilitiesProvider for StaticCapabilities {
    async fn capabilities(&self) -> GradientCapabilities {
        self.0.clone()
    }
}

/// Perform the full challenge-response handshake.
///
/// `peer_id`     - persistent UUID loaded from disk (or freshly generated on first start).
/// `peer_tokens` - `(peer_id, plaintext_token)` pairs from `GRADIENT_WORKER_PEERS`.
/// `capabilities`- advertised capabilities.
pub async fn perform_handshake(
    conn: &mut ProtoConnection,
    peer_id: String,
    peer_tokens: Vec<(String, String)>,
    capabilities: GradientCapabilities,
) -> Result<HandshakeResult> {
    let identity = WorkerIdentity {
        peer_id,
        peer_tokens,
    };
    let capabilities = StaticCapabilities(capabilities);
    let result = as_peer(conn.socket_mut(), &identity, &capabilities).await?;
    info!(
        server_version = result.server_version,
        client_version = PROTO_VERSION,
        authorized = result.authorized_peers.len(),
        failed = result.failed_peers.len(),
        "handshake successful"
    );
    for fp in &result.failed_peers {
        tracing::warn!(peer_id = %fp.peer_id, reason = %fp.reason, "peer auth failed");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_proto::messages::{ClientMessage, ServerMessage};
    use gradient_test_support::prelude::{MockProtoServer, MockServerConn};

    fn all_caps() -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: false,
            fetch: true,
            eval: true,
            build: true,
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
            cache: false,
        }
    }

    async fn run_server(
        mut sc: MockServerConn,
        challenge_peers: Vec<String>,
        response: ServerMessage,
    ) {
        // Receive InitConnection.
        let _ = sc.recv().await.unwrap();
        // Send AuthChallenge.
        sc.send(ServerMessage::AuthChallenge {
            peers: challenge_peers,
        })
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

        let mut conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
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

        let mut conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let err = perform_handshake(&mut conn, "wid".to_owned(), vec![], no_caps())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("banned"),
            "unexpected error: {err}"
        );
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

        let mut conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let err = perform_handshake(&mut conn, "wid".to_owned(), vec![], no_caps())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("bad token"),
            "unexpected error: {err}"
        );
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

        let mut conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
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
        let mut conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let _ = perform_handshake(&mut conn, "wid".to_owned(), peer_tokens, no_caps())
            .await
            .unwrap();

        server_task.await.unwrap();
    }
}
