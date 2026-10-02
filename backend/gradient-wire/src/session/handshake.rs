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
use crate::traits::{AuthOutcome, CapabilitiesProvider, PeerAuthority, PeerIdentity};

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

pub async fn as_authority<A>(
    socket: &mut ProtoSocket,
    authority: &A,
) -> anyhow::Result<HandshakeResult>
where
    A: PeerAuthority + ?Sized,
{
    let init = socket
        .recv_msg()
        .await
        .ok_or_else(|| anyhow::anyhow!("connection closed before InitConnection"))?;
    let greeted = match on_init_connection(Opening, init, PROTO_VERSION) {
        Ok(g) => g,
        Err(Intent::Reject { code, reason }) => {
            socket.send_reject(code, reason.clone()).await;
            anyhow::bail!("handshake rejected: {reason}");
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
    let tokens = match &auth_response {
        ClientMessage::AuthResponse { tokens } => tokens.clone(),
        _ => {
            let reason = "expected AuthResponse".to_string();
            socket.send_reject(400, reason.clone()).await;
            anyhow::bail!("{reason}");
        }
    };
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
            socket.send_reject(code, reason.clone()).await;
            anyhow::bail!("handshake rejected ({code}): {reason}");
        }
    };

    let negotiated = authority
        .negotiate(&greeted.peer_id, greeted.client_capabilities.clone())
        .await
        .context("negotiate")?;
    let authenticated = match on_auth_response(greeted, auth_response, negotiated.clone()) {
        Ok(a) => a,
        Err(Intent::Reject { code, reason }) => {
            socket.send_reject(code, reason.clone()).await;
            anyhow::bail!("auth response rejected: {reason}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::PROTO_VERSION;

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
}
