/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use gradient_wire::auth::verify_dialer_tokens;
use gradient_wire::messages::GradientCapabilities;
use gradient_wire::session::frame::HANDSHAKE_TIMEOUT;
use gradient_wire::session::handshake::{HandshakeResult, as_dialed, as_peer};
use gradient_wire::traits::{CapabilitiesProvider, DialerVerifier, PeerIdentity};
use tracing::info;

use super::{ProtoConnection, Unresponsive};

pub fn resolve_tokens_for_challenge(
    peer_tokens: &[(String, String)],
    challenged: &[String],
) -> Vec<(String, String)> {
    let wildcard_token: Option<&str> = peer_tokens
        .iter()
        .find(|(id, _)| id == "*")
        .map(|(_, t)| t.as_str());

    let mut result: Vec<(String, String)> = peer_tokens
        .iter()
        .filter(|(id, _)| id != "*" && challenged.contains(id))
        .cloned()
        .collect();

    if let Some(token) = wildcard_token {
        let covered: std::collections::HashSet<String> =
            result.iter().map(|(id, _)| id.clone()).collect();
        let extras: Vec<(String, String)> = challenged
            .iter()
            .filter(|pid| !covered.contains(*pid))
            .map(|pid| (pid.clone(), token.to_owned()))
            .collect();
        result.extend(extras);
    }

    result
}

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
        Ok(resolve_tokens_for_challenge(&self.peer_tokens, peers))
    }
}

struct StaticCapabilities(GradientCapabilities);

#[async_trait]
impl CapabilitiesProvider for StaticCapabilities {
    async fn capabilities(&self) -> GradientCapabilities {
        self.0.clone()
    }
}

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
    let result = answered_in_time(as_peer(conn.socket_mut(), &identity, &capabilities)).await?;
    info!(
        version = result.version,
        authorized = result.authorized_peers.len(),
        failed = result.failed_peers.len(),
        "handshake successful"
    );
    for fp in &result.failed_peers {
        tracing::warn!(peer_id = %fp.peer_id, reason = %fp.reason, "peer auth failed");
    }
    Ok(result)
}

async fn answered_in_time(
    handshake: impl Future<Output = Result<HandshakeResult>>,
) -> Result<HandshakeResult> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| Unresponsive)?
}

struct AcceptedServers(Option<Arc<Vec<(String, String)>>>);

#[async_trait]
impl DialerVerifier for AcceptedServers {
    async fn verify(&self, _worker_id: &str, tokens: &[(String, String)]) -> bool {
        let Some(accepted) = self.0.clone() else {
            return true;
        };

        let presented = tokens.to_vec();
        tokio::task::spawn_blocking(move || verify_dialer_tokens(&accepted, &presented))
            .await
            .unwrap_or(false)
    }
}

pub async fn perform_dialed_handshake(
    conn: &mut ProtoConnection,
    peer_id: String,
    accepted_server_tokens: Option<Vec<(String, String)>>,
    capabilities: GradientCapabilities,
) -> Result<HandshakeResult> {
    let identity = WorkerIdentity {
        peer_id,
        peer_tokens: Vec::new(),
    };
    let capabilities = StaticCapabilities(capabilities);
    let verifier = AcceptedServers(accepted_server_tokens.map(Arc::new));
    let result = answered_in_time(as_dialed(
        conn.socket_mut(),
        &identity,
        &capabilities,
        &verifier,
    ))
    .await?;
    info!(
        version = result.version,
        authorized = result.authorized_peers.len(),
        "server-dialed handshake successful"
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use gradient_wire::messages::{ClientMessage, ServerMessage};
    use gradient_wire::testing::{MockProtoServer, MockServerConn};

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
        let _ = sc.recv().await.unwrap();
        sc.send(ServerMessage::AuthChallenge {
            peers: challenge_peers,
        })
        .await
        .unwrap();
        let _ = sc.recv().await.unwrap();
        sc.send(response).await.unwrap();
    }

    #[tokio::test]
    async fn handshake_success() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let ack = ServerMessage::InitAck {
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

        assert_eq!(result.version, *gradient_wire::PROTO_VERSIONS.end());
        assert!(result.negotiated.eval);
        assert!(result.negotiated.build);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn a_server_silent_after_the_version_agreement_fails_the_handshake() {
        let server = MockProtoServer::bind().await;
        let (_silent, conn) = tokio::join!(
            server.accept(),
            crate::connection::ProtoConnection::open(server.url())
        );
        let mut conn = conn.unwrap();
        tokio::time::pause();

        let error = perform_handshake(&mut conn, "wid".to_owned(), vec![], no_caps())
            .await
            .unwrap_err();

        assert!(
            error.downcast_ref::<Unresponsive>().is_some(),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn handshake_reject_at_challenge() {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let _ = sc.recv().await.unwrap();
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
            let _ = sc.recv().await.unwrap();
            sc.send(ServerMessage::AuthChallenge { peers: vec![] })
                .await
                .unwrap();
            let _ = sc.recv().await.unwrap();
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
            let _ = sc.recv().await.unwrap();
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

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let _ = sc.recv().await.unwrap();

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

    #[test]
    fn resolve_tokens_explicit_only() {
        let tokens = vec![
            ("peer-a".to_owned(), "tok-a".to_owned()),
            ("peer-c".to_owned(), "tok-c".to_owned()),
        ];
        let challenged = vec!["peer-a".to_owned(), "peer-b".to_owned()];
        let result = resolve_tokens_for_challenge(&tokens, &challenged);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("peer-a".to_owned(), "tok-a".to_owned()));
    }

    #[test]
    fn resolve_tokens_wildcard_fills_gaps() {
        let tokens = vec![
            ("*".to_owned(), "wild".to_owned()),
            ("peer-a".to_owned(), "tok-a".to_owned()),
        ];
        let challenged = vec![
            "peer-a".to_owned(),
            "peer-b".to_owned(),
            "peer-c".to_owned(),
        ];
        let result = resolve_tokens_for_challenge(&tokens, &challenged);
        assert_eq!(result.len(), 3);

        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map["peer-a"], "tok-a");
        assert_eq!(map["peer-b"], "wild");
        assert_eq!(map["peer-c"], "wild");
    }

    #[test]
    fn resolve_tokens_empty_when_no_match() {
        let tokens = vec![("peer-x".to_owned(), "tok".to_owned())];
        let challenged = vec!["peer-y".to_owned()];
        let result = resolve_tokens_for_challenge(&tokens, &challenged);
        assert!(result.is_empty());
    }

    const SHA256_OF_T1: &str = "628b49d96dcde97a430dd4f597705899e09a968f793491e4b704cae33a40dc02";

    #[tokio::test]
    async fn without_an_accepted_tokens_file_every_server_is_accepted() {
        assert!(AcceptedServers(None).verify("w1", &[]).await);
    }

    #[tokio::test]
    async fn with_an_accepted_tokens_file_only_matching_tokens_pass() {
        let accepted = AcceptedServers(Some(Arc::new(vec![("p1".into(), SHA256_OF_T1.into())])));

        assert!(accepted.verify("w1", &[("p1".into(), "t1".into())]).await);
        assert!(!accepted.verify("w1", &[("p1".into(), "t2".into())]).await);
        assert!(!accepted.verify("w1", &[]).await);
    }
}
