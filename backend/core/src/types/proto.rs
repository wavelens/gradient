/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use rkyv::{Archive, Deserialize, Serialize};

/// Feature flags exchanged during the protocol handshake.
///
/// Each field represents one optional capability.  The client sends the flags
/// it supports in `ClientMessage::InitConnection`; the server responds with
/// only the flags it is willing to activate for this session in
/// `ServerMessage::InitAck`.  Unknown flags in a received message are always
/// treated as `false` — adding new fields is forwards-compatible.
///
/// All fields default to `false` so a zeroed struct is a valid
/// "no features" state.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub struct GradientCapabilities {
    /// Peer is the Gradient server itself (coordinator).
    /// Always `true` on the server side, always `false` for external workers.
    pub core: bool,
    /// Client supports federation — relaying work and NAR traffic between workers and servers.
    pub federate: bool,
    /// Client supports fetching flake inputs and pre-fetching sources.
    pub fetch: bool,
    /// Client supports Nix flake evaluation.
    pub eval: bool,
    /// Client supports executing Nix builds.
    pub build: bool,
    /// Client supports signing store paths and uploading signatures.
    pub sign: bool,
    /// Peer serves as a Nix binary cache.
    /// Set by the server when `GRADIENT_SERVE_CACHE=true`, never by workers.
    pub cache: bool,
}
