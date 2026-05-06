/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Alignment-safe rkyv decoders for the wire protocol.
//!
//! Inbound bytes from a WebSocket arrive in a `Vec<u8>` (or
//! `axum::body::Bytes` / `tokio_tungstenite::Message::Binary`) whose backing
//! allocation has no alignment guarantee beyond `align_of::<u8>() == 1`.
//! `rkyv::from_bytes`, however, requires the input slice to be aligned to the
//! archive's required alignment (16 bytes for our message types). Decoding a
//! bare `&[u8]` therefore fails non-deterministically depending on what the
//! allocator happened to return — typically presenting as a "malformed
//! message" warning shortly after a reconnect or on busy connections.
//!
//! These helpers copy inbound bytes into an [`AlignedVec<16>`] before
//! decoding, making `ClientMessage` / `ServerMessage` decoding deterministic
//! regardless of the source buffer's alignment. Every WebSocket-receive path
//! in the project must funnel through these helpers — open-coding
//! `rkyv::from_bytes` on raw network bytes is the bug they exist to prevent.

use rkyv::rancor::Error as RkyvError;
use rkyv::util::AlignedVec;

use super::{ClientMessage, ServerMessage};

/// Decode a [`ClientMessage`] from inbound wire bytes of arbitrary alignment.
pub fn decode_client_message(bytes: &[u8]) -> Result<ClientMessage, RkyvError> {
    let mut aligned = AlignedVec::<16>::new();
    aligned.extend_from_slice(bytes);
    rkyv::from_bytes::<ClientMessage, RkyvError>(&aligned)
}

/// Decode a [`ServerMessage`] from inbound wire bytes of arbitrary alignment.
pub fn decode_server_message(bytes: &[u8]) -> Result<ServerMessage, RkyvError> {
    let mut aligned = AlignedVec::<16>::new();
    aligned.extend_from_slice(bytes);
    rkyv::from_bytes::<ServerMessage, RkyvError>(&aligned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::PROTO_VERSION;
    use gradient_core::types::proto::GradientCapabilities;
    use rkyv::util::AlignedVec;

    /// Construct a deliberately misaligned slice that points at a
    /// `base_ptr + 1` offset within a 16-byte-aligned allocation. The
    /// returned slice's start address is therefore guaranteed to violate
    /// rkyv's 16-byte alignment requirement, which is the exact condition
    /// inbound WebSocket bytes can land in when the allocator hands back a
    /// `Vec<u8>` that does not happen to begin on a 16-byte boundary.
    fn misalign(bytes: &[u8]) -> AlignedVec<16> {
        let mut padded = AlignedVec::<16>::new();
        padded.push(0);
        padded.extend_from_slice(bytes);
        padded
    }

    /// Decoding a `ClientMessage` from a misaligned source buffer must
    /// succeed. This is the regression test for the server-side
    /// "failed to deserialize client message" warning that surfaced when
    /// inbound axum/tungstenite buffers happened to land at an unaligned
    /// allocator address.
    #[test]
    fn decode_client_message_handles_misaligned_input() {
        let original = ClientMessage::InitConnection {
            version: PROTO_VERSION,
            capabilities: GradientCapabilities::default(),
            id: "550e8400-e29b-41d4-a716-446655440000".into(),
        };
        let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();

        let padded = misalign(&bytes);
        let unaligned: &[u8] = &padded[1..];
        assert_ne!(
            unaligned.as_ptr() as usize % 16,
            0,
            "test buffer must actually be misaligned to be a meaningful regression"
        );

        let decoded = decode_client_message(unaligned).expect("misaligned decode must succeed");
        assert_eq!(decoded, original);
    }

    /// Symmetric coverage for the worker's inbound path.
    #[test]
    fn decode_server_message_handles_misaligned_input() {
        let original = ServerMessage::InitAck {
            version: PROTO_VERSION,
            capabilities: GradientCapabilities::default(),
            authorized_peers: vec!["peer-1".into()],
            failed_peers: vec![],
        };
        let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();

        let padded = misalign(&bytes);
        let unaligned: &[u8] = &padded[1..];
        assert_ne!(
            unaligned.as_ptr() as usize % 16,
            0,
            "test buffer must actually be misaligned to be a meaningful regression"
        );

        let decoded = decode_server_message(unaligned).expect("misaligned decode must succeed");
        assert_eq!(decoded, original);
    }
}
