/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;

use crate::messages::{
    ClientMessage, FailedPeer, GradientCapabilities, PROTO_VERSION, ServerMessage,
};
use crate::session::frame::{ProtoSocket, recv_server_msg, send_client_msg};
use crate::traits::{
    AuthOutcome, CapabilitiesProvider, DialerAuthority, DialerVerifier, PeerAuthority, PeerIdentity,
};

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct Opening;

#[derive(Debug, PartialEq, Clone)]
pub struct Greeted {
    pub peer_id: String,
    pub client_capabilities: GradientCapabilities,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Authenticated {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Registered {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Intent {
    Send(Box<ServerMessage>),
    Advance,
    Reject { code: u16, reason: String },
}

pub fn on_init_connection(
    _: Opening,
    msg: ClientMessage,
    expected_version: u16,
) -> Result<Greeted, Intent> {
    let ClientMessage::InitConnection {
        version,
        capabilities,
        id,
    } = msg
    else {
        return Err(Intent::Reject {
            code: 400,
            reason: "expected InitConnection".into(),
        });
    };
    if version != expected_version {
        return Err(Intent::Reject {
            code: 400,
            reason: format!(
                "protocol version mismatch: peer={version}, expected={expected_version}"
            ),
        });
    }
    Ok(Greeted {
        peer_id: id,
        client_capabilities: capabilities,
    })
}

/// The caller must validate the token plaintexts against the stored argon2 hash before this call.
/// `negotiated` is the intersection of advertised and authorized capabilities.
pub fn on_auth_response(
    greeted: Greeted,
    msg: ClientMessage,
    negotiated: GradientCapabilities,
) -> Result<Authenticated, Intent> {
    let ClientMessage::AuthResponse { .. } = msg else {
        return Err(Intent::Reject {
            code: 400,
            reason: "expected AuthResponse".into(),
        });
    };
    Ok(Authenticated {
        peer_id: greeted.peer_id,
        negotiated,
    })
}

pub fn to_registered(auth: Authenticated) -> Registered {
    Registered {
        peer_id: auth.peer_id,
        negotiated: auth.negotiated,
    }
}

#[derive(Debug, Clone)]
pub struct HandshakeResult {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
    pub authorized_peers: Vec<String>,
    pub failed_peers: Vec<FailedPeer>,
    pub server_version: u16,
}

pub async fn as_peer<I, C>(
    socket: &mut ProtoSocket,
    identity: &I,
    capabilities: &C,
) -> anyhow::Result<HandshakeResult>
where
    I: PeerIdentity + ?Sized,
    C: CapabilitiesProvider + ?Sized,
{
    let caps = capabilities.capabilities().await;
    send_client_msg(
        socket,
        &ClientMessage::InitConnection {
            version: PROTO_VERSION,
            capabilities: caps.clone(),
            id: identity.peer_id(),
        },
    )
    .await?;

    let challenge = recv_server_msg(socket).await?;
    let challenged = match challenge {
        ServerMessage::AuthChallenge { peers } => peers,
        ServerMessage::Reject { code, reason } => {
            anyhow::bail!("server rejected connection (code {code}): {reason}");
        }
        other => anyhow::bail!("expected AuthChallenge, got: {other:?}"),
    };

    let tokens = identity.tokens_for(&challenged).await?;
    send_client_msg(socket, &ClientMessage::AuthResponse { tokens }).await?;

    let ack = recv_server_msg(socket).await?;
    let ServerMessage::InitAck {
        version,
        capabilities: negotiated,
        authorized_peers,
        failed_peers,
    } = ack
    else {
        if let ServerMessage::Reject { code, reason } = ack {
            anyhow::bail!("server rejected connection (code {code}): {reason}");
        }
        anyhow::bail!("expected InitAck, got: {ack:?}");
    };

    Ok(HandshakeResult {
        peer_id: identity.peer_id(),
        negotiated,
        authorized_peers,
        failed_peers,
        server_version: version,
    })
}

pub const UNKNOWN_WORKER_OR_WRONG_TOKEN: &str = "unknown worker id or wrong token";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("handshake rejected ({code}): {reason}")]
pub struct Rejected {
    pub code: u16,
    pub reason: String,
    pub claimed: Option<String>,
}

#[derive(Clone)]
pub struct DialerCredentials {
    pub worker_id: String,
    pub tokens: Vec<(String, String)>,
}

pub struct DialerClaim {
    pub worker_id: String,
    pub tokens: Vec<(String, String)>,
}

pub fn on_authenticate(msg: ServerMessage, expected_version: u16) -> Result<DialerClaim, Intent> {
    let ServerMessage::Authenticate {
        version,
        worker_id,
        tokens,
    } = msg
    else {
        return Err(Intent::Reject {
            code: 400,
            reason: "expected Authenticate".into(),
        });
    };
    if version != expected_version {
        return Err(Intent::Reject {
            code: 400,
            reason: format!(
                "protocol version mismatch: peer={version}, expected={expected_version}"
            ),
        });
    }

    Ok(DialerClaim { worker_id, tokens })
}

async fn reject_peer(
    socket: &mut ProtoSocket,
    code: u16,
    reason: String,
    claimed: Option<String>,
) -> anyhow::Error {
    socket.send_reject(code, reason.clone()).await;
    Rejected {
        code,
        reason,
        claimed,
    }
    .into()
}

pub async fn reject_dialer(socket: &mut ProtoSocket, code: u16, reason: String) -> anyhow::Error {
    let _ = socket
        .send_client_msg(&ClientMessage::Reject {
            code,
            reason: reason.clone(),
        })
        .await;
    Rejected {
        code,
        reason,
        claimed: None,
    }
    .into()
}

fn claimed_id(msg: &ClientMessage) -> Option<String> {
    match msg {
        ClientMessage::InitConnection { id, .. } => Some(id.clone()),
        _ => None,
    }
}

pub async fn as_authority<A>(
    socket: &mut ProtoSocket,
    authority: &A,
) -> anyhow::Result<HandshakeResult>
where
    A: PeerAuthority + ?Sized,
{
    let greeting = socket
        .recv_msg()
        .await
        .ok_or_else(|| anyhow::anyhow!("connection closed before InitConnection"))?;
    as_authority_with_greeting(socket, greeting, authority).await
}

pub async fn as_authority_with_greeting<A>(
    socket: &mut ProtoSocket,
    greeting: ClientMessage,
    authority: &A,
) -> anyhow::Result<HandshakeResult>
where
    A: PeerAuthority + ?Sized,
{
    let claimed = claimed_id(&greeting);
    let greeted = match on_init_connection(Opening, greeting, PROTO_VERSION) {
        Ok(g) => g,
        Err(Intent::Reject { code, reason }) => {
            return Err(reject_peer(socket, code, reason, claimed).await);
        }
        Err(other) => anyhow::bail!("unexpected intent during init: {other:?}"),
    };

    let (challenge, challenge_peers) = authority
        .challenge(&greeted.peer_id)
        .await
        .context("challenge")?;
    socket
        .send_msg(&ServerMessage::AuthChallenge {
            peers: challenge_peers,
        })
        .await
        .map_err(|_| anyhow::anyhow!("send AuthChallenge"))?;

    let auth_response = socket
        .recv_msg()
        .await
        .ok_or_else(|| anyhow::anyhow!("connection closed before AuthResponse"))?;
    let ClientMessage::AuthResponse { tokens } = &auth_response else {
        let reason = "expected AuthResponse".to_string();
        return Err(reject_peer(socket, 400, reason, Some(greeted.peer_id.clone())).await);
    };
    let tokens = tokens.clone();
    let (authorized_peers, failed_peers) = match authority
        .authorize(&greeted.peer_id, challenge, &tokens)
        .await
        .context("authorize")?
    {
        AuthOutcome::Accept {
            authorized_peers,
            failed_peers,
        } => (authorized_peers, failed_peers),
        AuthOutcome::Reject { code, reason } => {
            return Err(reject_peer(socket, code, reason, Some(greeted.peer_id.clone())).await);
        }
    };

    let negotiated = authority
        .negotiate(&greeted.peer_id, greeted.client_capabilities.clone())
        .await
        .context("negotiate")?;
    let claimed = Some(greeted.peer_id.clone());
    let authenticated = match on_auth_response(greeted, auth_response, negotiated.clone()) {
        Ok(a) => a,
        Err(Intent::Reject { code, reason }) => {
            return Err(reject_peer(socket, code, reason, claimed).await);
        }
        Err(other) => anyhow::bail!("unexpected intent during auth: {other:?}"),
    };

    socket
        .send_msg(&ServerMessage::InitAck {
            version: PROTO_VERSION,
            capabilities: negotiated.clone(),
            authorized_peers: authorized_peers.clone(),
            failed_peers: failed_peers.clone(),
        })
        .await
        .map_err(|_| anyhow::anyhow!("send InitAck"))?;

    let registered = to_registered(authenticated);
    Ok(HandshakeResult {
        peer_id: registered.peer_id,
        negotiated: registered.negotiated,
        authorized_peers,
        failed_peers,
        server_version: PROTO_VERSION,
    })
}

pub async fn as_dialer<A>(
    socket: &mut ProtoSocket,
    credentials: &DialerCredentials,
    authority: &A,
) -> anyhow::Result<HandshakeResult>
where
    A: DialerAuthority + ?Sized,
{
    socket
        .send_msg(&ServerMessage::Authenticate {
            version: PROTO_VERSION,
            worker_id: credentials.worker_id.clone(),
            tokens: credentials.tokens.clone(),
        })
        .await
        .map_err(|_| anyhow::anyhow!("send Authenticate"))?;

    let greeted = greet_dialed_worker(socket, &credentials.worker_id).await?;
    let (authorized_peers, failed_peers) = match authority
        .admit(&credentials.worker_id)
        .await
        .context("admit")?
    {
        AuthOutcome::Accept {
            authorized_peers,
            failed_peers,
        } => (authorized_peers, failed_peers),
        AuthOutcome::Reject { code, reason } => {
            return Err(reject_peer(socket, code, reason, None).await);
        }
    };

    let negotiated = authority
        .negotiate(&credentials.worker_id, greeted.client_capabilities)
        .await
        .context("negotiate")?;
    socket
        .send_msg(&ServerMessage::InitAck {
            version: PROTO_VERSION,
            capabilities: negotiated.clone(),
            authorized_peers: authorized_peers.clone(),
            failed_peers: failed_peers.clone(),
        })
        .await
        .map_err(|_| anyhow::anyhow!("send InitAck"))?;

    Ok(HandshakeResult {
        peer_id: credentials.worker_id.clone(),
        negotiated,
        authorized_peers,
        failed_peers,
        server_version: PROTO_VERSION,
    })
}

async fn greet_dialed_worker(socket: &mut ProtoSocket, worker_id: &str) -> anyhow::Result<Greeted> {
    let init = match socket.recv_msg().await {
        None => anyhow::bail!("connection closed before InitConnection"),
        Some(ClientMessage::Reject { code, reason }) => {
            return Err(Rejected {
                code,
                reason,
                claimed: None,
            }
            .into());
        }
        Some(init) => init,
    };
    let greeted = match on_init_connection(Opening, init, PROTO_VERSION) {
        Ok(greeted) => greeted,
        Err(Intent::Reject { code, reason }) => {
            return Err(reject_peer(socket, code, reason, None).await);
        }
        Err(other) => anyhow::bail!("unexpected intent during init: {other:?}"),
    };
    if greeted.peer_id != worker_id {
        let reason = format!("dialed worker answered as {}", greeted.peer_id);
        return Err(reject_peer(socket, 400, reason, None).await);
    }

    Ok(greeted)
}

pub async fn as_dialed<I, C, V>(
    socket: &mut ProtoSocket,
    identity: &I,
    capabilities: &C,
    verifier: &V,
) -> anyhow::Result<HandshakeResult>
where
    I: PeerIdentity + ?Sized,
    C: CapabilitiesProvider + ?Sized,
    V: DialerVerifier + ?Sized,
{
    let first = recv_server_msg(socket).await?;
    let claim = match on_authenticate(first, PROTO_VERSION) {
        Ok(claim) => claim,
        Err(Intent::Reject { code, reason }) => {
            return Err(reject_dialer(socket, code, reason).await);
        }
        Err(other) => anyhow::bail!("unexpected intent during Authenticate: {other:?}"),
    };
    let known = claim.worker_id == identity.peer_id();
    if !known || !verifier.verify(&claim.worker_id, &claim.tokens).await {
        let reason = UNKNOWN_WORKER_OR_WRONG_TOKEN.to_string();
        return Err(reject_dialer(socket, 401, reason).await);
    }

    answer_dialer(socket, identity, capabilities).await
}

pub async fn answer_dialer<I, C>(
    socket: &mut ProtoSocket,
    identity: &I,
    capabilities: &C,
) -> anyhow::Result<HandshakeResult>
where
    I: PeerIdentity + ?Sized,
    C: CapabilitiesProvider + ?Sized,
{
    send_client_msg(
        socket,
        &ClientMessage::InitConnection {
            version: PROTO_VERSION,
            capabilities: capabilities.capabilities().await,
            id: identity.peer_id(),
        },
    )
    .await?;

    match recv_server_msg(socket).await? {
        ServerMessage::InitAck {
            version,
            capabilities: negotiated,
            authorized_peers,
            failed_peers,
        } => Ok(HandshakeResult {
            peer_id: identity.peer_id(),
            negotiated,
            authorized_peers,
            failed_peers,
            server_version: version,
        }),
        ServerMessage::Reject { code, reason } => Err(Rejected {
            code,
            reason,
            claimed: None,
        }
        .into()),
        other => anyhow::bail!("expected InitAck, got {}", other.variant_name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::PROTO_VERSION;
    use crate::session::frame::FirstMessage;
    use crate::testing::loopback;

    #[test]
    fn opening_accepts_well_formed_init_connection() {
        let result = on_init_connection(
            Opening,
            ClientMessage::InitConnection {
                version: PROTO_VERSION,
                capabilities: GradientCapabilities::default(),
                id: "peer-1".into(),
            },
            PROTO_VERSION,
        );
        let g = result.expect("expected Greeted");
        assert_eq!(g.peer_id, "peer-1");
    }

    #[test]
    fn opening_rejects_version_mismatch() {
        let result = on_init_connection(
            Opening,
            ClientMessage::InitConnection {
                version: 999,
                capabilities: GradientCapabilities::default(),
                id: "peer-1".into(),
            },
            PROTO_VERSION,
        );
        let Err(Intent::Reject { code, reason }) = result else {
            panic!("expected Reject");
        };
        assert_eq!(code, 400);
        assert!(reason.contains("version mismatch"));
    }

    #[test]
    fn opening_rejects_wrong_variant() {
        let result = on_init_connection(Opening, ClientMessage::Draining, PROTO_VERSION);
        assert!(matches!(result, Err(Intent::Reject { .. })));
    }

    #[test]
    fn greeted_to_authenticated_accepts_valid_auth_response() {
        let greeted = Greeted {
            peer_id: "peer-1".into(),
            client_capabilities: GradientCapabilities {
                build: true,
                ..Default::default()
            },
        };
        let negotiated = GradientCapabilities {
            build: true,
            ..Default::default()
        };
        let result = on_auth_response(
            greeted.clone(),
            ClientMessage::AuthResponse {
                tokens: vec![("peer-1".into(), "plaintext".into())],
            },
            negotiated.clone(),
        );
        let a = result.expect("expected Authenticated");
        assert_eq!(a.peer_id, "peer-1");
        assert_eq!(a.negotiated, negotiated);
    }

    #[test]
    fn greeted_rejects_wrong_message_variant() {
        let greeted = Greeted {
            peer_id: "peer-1".into(),
            client_capabilities: GradientCapabilities::default(),
        };
        let result = on_auth_response(
            greeted,
            ClientMessage::Draining,
            GradientCapabilities::default(),
        );
        assert!(matches!(result, Err(Intent::Reject { .. })));
    }

    struct Admits(Vec<String>);

    #[async_trait::async_trait]
    impl DialerAuthority for Admits {
        async fn admit(&self, _: &str) -> anyhow::Result<AuthOutcome> {
            Ok(AuthOutcome::Accept {
                authorized_peers: self.0.clone(),
                failed_peers: vec![],
            })
        }

        async fn negotiate(
            &self,
            _: &str,
            client: GradientCapabilities,
        ) -> anyhow::Result<GradientCapabilities> {
            Ok(client)
        }
    }

    struct Worker(&'static str);

    #[async_trait::async_trait]
    impl PeerIdentity for Worker {
        fn peer_id(&self) -> String {
            self.0.into()
        }

        async fn tokens_for(&self, _: &[String]) -> anyhow::Result<Vec<(String, String)>> {
            Ok(vec![])
        }
    }

    #[async_trait::async_trait]
    impl CapabilitiesProvider for Worker {
        async fn capabilities(&self) -> GradientCapabilities {
            GradientCapabilities {
                build: true,
                ..Default::default()
            }
        }
    }

    struct Expects(Vec<(String, String)>);

    #[async_trait::async_trait]
    impl DialerVerifier for Expects {
        async fn verify(&self, _: &str, tokens: &[(String, String)]) -> bool {
            tokens == self.0.as_slice()
        }
    }

    struct NeverAsked;

    #[async_trait::async_trait]
    impl PeerAuthority for NeverAsked {
        type Challenge = ();

        async fn challenge(&self, _: &str) -> anyhow::Result<((), Vec<String>)> {
            anyhow::bail!("not reached")
        }

        async fn authorize(
            &self,
            _: &str,
            _: (),
            _: &[(String, String)],
        ) -> anyhow::Result<AuthOutcome> {
            anyhow::bail!("not reached")
        }

        async fn negotiate(
            &self,
            _: &str,
            _: GradientCapabilities,
        ) -> anyhow::Result<GradientCapabilities> {
            anyhow::bail!("not reached")
        }
    }

    fn credentials() -> DialerCredentials {
        DialerCredentials {
            worker_id: "w1".into(),
            tokens: vec![("p1".into(), "s3cret".into())],
        }
    }

    fn rejection(outcome: anyhow::Result<HandshakeResult>) -> Rejected {
        let Err(error) = outcome else {
            panic!("expected a rejection");
        };
        error.downcast::<Rejected>().expect("a Rejected error")
    }

    #[tokio::test]
    async fn a_server_dialed_session_authenticates_with_the_dialers_tokens() {
        let (mut accepted, mut dialing) = loopback().await;
        let (credentials, authority) = (credentials(), Admits(vec!["p1".into()]));
        let verifier = Expects(credentials.tokens.clone());
        let (dialer, dialed) = tokio::join!(
            as_dialer(&mut dialing, &credentials, &authority),
            as_dialed(&mut accepted, &Worker("w1"), &Worker("w1"), &verifier),
        );

        let dialer = dialer.expect("dialer handshake");
        let dialed = dialed.expect("dialed handshake");
        assert_eq!(dialer.peer_id, "w1");
        assert!(dialer.negotiated.build);
        assert_eq!(dialed.authorized_peers, vec!["p1".to_string()]);
    }

    #[tokio::test]
    async fn a_wrong_token_is_answered_with_401_and_nothing_else() {
        let (accepted, mut dialing) = loopback().await;
        let worker = async move {
            let mut accepted = accepted;
            let wrong = Expects(vec![("p1".into(), "other".into())]);
            let outcome = as_dialed(&mut accepted, &Worker("w1"), &Worker("w1"), &wrong).await;
            drop(accepted);
            outcome
        };
        let server = async {
            dialing
                .send_msg(&ServerMessage::Authenticate {
                    version: PROTO_VERSION,
                    worker_id: "w1".into(),
                    tokens: credentials().tokens,
                })
                .await
                .expect("send Authenticate");
            (dialing.recv_msg().await, dialing.recv_msg().await)
        };

        let (outcome, (first, second)) = tokio::join!(worker, server);
        assert_eq!(rejection(outcome).code, 401);
        assert_eq!(
            first,
            Some(ClientMessage::Reject {
                code: 401,
                reason: "unknown worker id or wrong token".into(),
            })
        );
        assert_eq!(second, None);
    }

    #[tokio::test]
    async fn an_unknown_worker_id_is_rejected_with_401() {
        let (mut accepted, mut dialing) = loopback().await;
        let (credentials, authority) = (credentials(), Admits(vec![]));
        let verifier = Expects(credentials.tokens.clone());
        let (dialer, _) = tokio::join!(
            as_dialer(&mut dialing, &credentials, &authority),
            as_dialed(&mut accepted, &Worker("w2"), &Worker("w2"), &verifier),
        );

        assert_eq!(rejection(dialer).code, 401);
    }

    #[tokio::test]
    async fn an_authenticate_from_another_protocol_version_is_rejected_with_400() {
        let (mut accepted, mut dialing) = loopback().await;
        let server = async {
            dialing
                .send_msg(&ServerMessage::Authenticate {
                    version: PROTO_VERSION + 1,
                    worker_id: "w1".into(),
                    tokens: vec![],
                })
                .await
                .expect("send Authenticate");
            dialing.recv_msg().await
        };

        let verifier = Expects(vec![]);
        let (outcome, reply) = tokio::join!(
            as_dialed(&mut accepted, &Worker("w1"), &Worker("w1"), &verifier),
            server
        );
        assert_eq!(rejection(outcome).code, 400);
        assert!(matches!(
            reply,
            Some(ClientMessage::Reject { code: 400, .. })
        ));
    }

    #[tokio::test]
    async fn a_worker_on_an_older_protocol_is_rejected_with_400_by_the_dialer() {
        let (mut accepted, mut dialing) = loopback().await;
        let old_worker = async {
            accepted
                .send_client_msg(&ClientMessage::InitConnection {
                    version: PROTO_VERSION - 1,
                    capabilities: GradientCapabilities::default(),
                    id: "w1".into(),
                })
                .await
                .expect("send InitConnection");
            let _authenticate = accepted.recv_server_msg().await;
            accepted.recv_server_msg().await
        };

        let (credentials, authority) = (credentials(), Admits(vec![]));
        let (dialer, reply) = tokio::join!(
            as_dialer(&mut dialing, &credentials, &authority),
            old_worker
        );
        assert_eq!(rejection(dialer).code, 400);
        assert!(matches!(
            reply,
            Some(ServerMessage::Reject { code: 400, .. })
        ));
    }

    #[tokio::test]
    async fn a_rejected_worker_is_named_by_the_id_it_claimed() {
        let (mut accepted, mut dialing) = loopback().await;
        let worker = async {
            dialing
                .send_client_msg(&ClientMessage::InitConnection {
                    version: PROTO_VERSION + 1,
                    capabilities: GradientCapabilities::default(),
                    id: "w1".into(),
                })
                .await
                .expect("send InitConnection");
        };

        let (outcome, ()) = tokio::join!(as_authority(&mut accepted, &NeverAsked), worker);
        let rejected = rejection(outcome);
        assert_eq!(
            (rejected.code, rejected.claimed.as_deref()),
            (400, Some("w1"))
        );
    }

    #[tokio::test]
    async fn a_dialed_side_that_checks_the_tokens_itself_answers_the_dialer() {
        let (mut accepted, mut dialing) = loopback().await;
        let dialed = async {
            let Some(FirstMessage::Server(first)) = accepted.recv_first_message().await else {
                panic!("expected Authenticate first");
            };
            let claim = on_authenticate(first, PROTO_VERSION).expect("current version");
            assert_eq!(claim.tokens, credentials().tokens);
            answer_dialer(&mut accepted, &Worker("w1"), &Worker("w1")).await
        };

        let (credentials, authority) = (credentials(), Admits(vec!["p1".into()]));
        let (dialer, dialed) =
            tokio::join!(as_dialer(&mut dialing, &credentials, &authority), dialed);
        assert_eq!(dialer.expect("dialer handshake").peer_id, "w1");
        assert_eq!(
            dialed.expect("dialed handshake").authorized_peers,
            vec!["p1".to_string()]
        );
    }

    #[tokio::test]
    async fn a_greeting_read_by_the_caller_is_judged_like_one_read_by_the_authority() {
        let (mut accepted, mut dialing) = loopback().await;
        let worker = async {
            dialing
                .send_client_msg(&ClientMessage::InitConnection {
                    version: PROTO_VERSION + 1,
                    capabilities: GradientCapabilities::default(),
                    id: "w1".into(),
                })
                .await
                .expect("send InitConnection");
        };
        let authority = async {
            let Some(FirstMessage::Worker(greeting)) = accepted.recv_first_message().await else {
                panic!("expected InitConnection first");
            };
            as_authority_with_greeting(&mut accepted, greeting, &NeverAsked).await
        };

        let (outcome, ()) = tokio::join!(authority, worker);
        let rejected = rejection(outcome);
        assert_eq!(
            (rejected.code, rejected.claimed.as_deref()),
            (400, Some("w1"))
        );
    }
}
