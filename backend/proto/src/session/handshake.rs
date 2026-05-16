/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure handshake state machine.
//!
//! Models the four states of an inbound proto handshake:
//! `Opening → Greeted → Authenticated → Registered`. The FSM has no I/O
//! dependency — it only sequences which `ClientMessage`/`ServerMessage`
//! pairs are valid at each step. Drivers (e.g. `proto::server::accept` or
//! gradient-server's existing session handler) feed it observed messages
//! and act on its emitted intent.

use crate::messages::{ClientMessage, GradientCapabilities, ServerMessage};

/// State markers — zero-sized; the FSM is encoded entirely in the type.
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
