/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure handshake state machine.
//!
//! Models the four states of an inbound proto handshake:
//! `Opening → Greeted → Authenticated → Registered`. The FSM has no I/O
//! dependency - it only sequences which `ClientMessage`/`ServerMessage`
//! pairs are valid at each step. Drivers (e.g. `proto::server::accept` or
//! gradient-server's existing session handler) feed it observed messages
//! and act on its emitted intent.

use anyhow::Context;

use crate::messages::{
    ClientMessage, FailedPeer, GradientCapabilities, PROTO_VERSION, ServerMessage,
};
use crate::session::frame::{ProtoSocket, recv_server_msg, send_client_msg};
use crate::traits::{CapabilitiesProvider, PeerAuthority, PeerIdentity, SessionFactory};

/// State markers - zero-sized; the FSM is encoded entirely in the type.
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

/// Emitted by the FSM as it advances. Drivers translate these into wire I/O.
#[derive(Debug, PartialEq, Clone)]
pub enum Intent {
    /// Send this message to the peer. Boxed to keep `Intent` small even
    /// though some `ServerMessage` variants (e.g. `NarPush`) are sizeable.
    Send(Box<ServerMessage>),
    /// Advance state silently (no message emitted).
    Advance,
    /// Reject the peer with an error reason; the driver closes the socket.
    Reject(String),
}

/// Pure transition: `Opening` on receipt of `InitConnection`.
///
/// Returns either the new `Greeted` state or a `Reject` intent if the
/// message is malformed (wrong variant, version mismatch, …).
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
        return Err(Intent::Reject("expected InitConnection".into()));
    };
    if version != expected_version {
        return Err(Intent::Reject(format!(
            "protocol version mismatch: peer={version}, expected={expected_version}"
        )));
    }
    Ok(Greeted {
        peer_id: id,
        client_capabilities: capabilities,
    })
}

/// Pure transition: `Greeted` on receipt of `AuthResponse`. The caller is
/// responsible for having validated the token plaintexts against the peer's
/// stored argon2 hash before calling this. `negotiated` is the capabilities
/// set the caller has decided on (intersection of advertised and authorized).
pub fn on_auth_response(
    greeted: Greeted,
    msg: ClientMessage,
    negotiated: GradientCapabilities,
) -> Result<Authenticated, Intent> {
    let ClientMessage::AuthResponse { .. } = msg else {
        return Err(Intent::Reject("expected AuthResponse".into()));
    };
    Ok(Authenticated {
        peer_id: greeted.peer_id,
        negotiated,
    })
}

/// Pure transition: `Authenticated → Registered` after the driver has sent
/// `InitAck` and recorded the peer in any session registry it maintains.
pub fn to_registered(auth: Authenticated) -> Registered {
    Registered {
        peer_id: auth.peer_id,
        negotiated: auth.negotiated,
    }
}

/// Result returned by both handshake roles on success.
#[derive(Debug, Clone)]
pub struct HandshakeResult {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
    pub authorized_peers: Vec<String>,
    pub failed_peers: Vec<FailedPeer>,
    pub server_version: u16,
}

/// Run the peer side of the handshake on an established socket.
/// Sends `InitConnection`, receives `AuthChallenge`, sends `AuthResponse`
/// with tokens for the challenged peers, receives `InitAck`.
///
/// Used by:
/// - gradient-worker dialing gradient-server (worker→server, standard).
/// - gradient-worker accepting from gradient-server (server→worker, discoverable mode).
/// - gradient-proxy dialing its upstream gradient-server (proxy→server).
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

/// Run the authority side of the handshake on an established socket.
/// Receives `InitConnection`, sends `AuthChallenge` with the list of peers
/// the authority wants verified, receives `AuthResponse`, validates tokens,
/// sends `InitAck`.
///
/// Used by:
/// - gradient-server accepting worker connections (axum-WS path).
/// - gradient-server dialing discoverable workers.
/// - gradient-proxy accepting backend worker connections.
///
/// Returns on success with a populated `HandshakeResult`. On any failure the
/// driver sends an appropriate `ServerMessage::Reject` and returns Err.
pub async fn as_authority<A, F>(
    socket: &mut ProtoSocket,
    authority: &A,
    factory: &F,
) -> anyhow::Result<HandshakeResult>
where
    A: PeerAuthority + ?Sized,
    F: SessionFactory + ?Sized,
{
    let init = socket
        .recv_msg()
        .await
        .ok_or_else(|| anyhow::anyhow!("connection closed before InitConnection"))?;
    let greeted = match on_init_connection(Opening, init, PROTO_VERSION) {
        Ok(g) => g,
        Err(Intent::Reject(reason)) => {
            socket.send_reject(400, reason.clone()).await;
            anyhow::bail!("handshake rejected: {reason}");
        }
        Err(other) => anyhow::bail!("unexpected intent during init: {other:?}"),
    };

    let challenge_peers = authority.challenge_peers(&greeted.peer_id).await?;
    if challenge_peers.is_empty() {
        let reason = format!("unknown peer {}", greeted.peer_id);
        socket.send_reject(401, reason.clone()).await;
        anyhow::bail!("{reason}");
    }
    let allowed = authority.allowed_capabilities(&greeted.peer_id).await?;

    socket
        .send_msg(&ServerMessage::AuthChallenge {
            peers: challenge_peers.clone(),
        })
        .await
        .map_err(|_| anyhow::anyhow!("send AuthChallenge"))?;

    let auth_response = socket
        .recv_msg()
        .await
        .ok_or_else(|| anyhow::anyhow!("connection closed before AuthResponse"))?;
    let tokens = match &auth_response {
        ClientMessage::AuthResponse { tokens } => tokens.clone(),
        other => anyhow::bail!("expected AuthResponse, got: {other:?}"),
    };
    let (authorized_peers, failed_peers) = authority
        .validate_tokens(&challenge_peers, &tokens)
        .await
        .context("validate_tokens")?;
    if authorized_peers.is_empty() {
        let reason = "no claimed peers validated".to_string();
        socket.send_reject(401, reason.clone()).await;
        anyhow::bail!("{reason}");
    }
    let negotiated = intersect_capabilities(&greeted.client_capabilities, &allowed);
    let authenticated = match on_auth_response(greeted, auth_response, negotiated.clone()) {
        Ok(a) => a,
        Err(Intent::Reject(reason)) => {
            socket.send_reject(400, reason.clone()).await;
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
    factory
        .on_registered(registered.peer_id.clone(), registered.negotiated.clone())
        .await
        .context("factory.on_registered")?;

    Ok(HandshakeResult {
        peer_id: registered.peer_id,
        negotiated: registered.negotiated,
        authorized_peers,
        failed_peers,
        server_version: PROTO_VERSION,
    })
}

fn intersect_capabilities(
    a: &GradientCapabilities,
    b: &GradientCapabilities,
) -> GradientCapabilities {
    GradientCapabilities {
        core: a.core && b.core,
        federate: a.federate && b.federate,
        fetch: a.fetch && b.fetch,
        eval: a.eval && b.eval,
        build: a.build && b.build,
        cache: a.cache && b.cache,
    }
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
        let Err(Intent::Reject(reason)) = result else {
            panic!("expected Reject");
        };
        assert!(reason.contains("version mismatch"));
    }

    #[test]
    fn opening_rejects_wrong_variant() {
        let result = on_init_connection(Opening, ClientMessage::Draining, PROTO_VERSION);
        assert!(matches!(result, Err(Intent::Reject(_))));
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
        assert!(matches!(result, Err(Intent::Reject(_))));
    }

    #[test]
    fn authenticated_to_registered_is_idempotent_carry() {
        let auth = Authenticated {
            peer_id: "peer-1".into(),
            negotiated: GradientCapabilities {
                eval: true,
                ..Default::default()
            },
        };
        let r = to_registered(auth.clone());
        assert_eq!(r.peer_id, auth.peer_id);
        assert_eq!(r.negotiated, auth.negotiated);
    }
}
