/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The Gradient wire protocol, shared by gradient-server, gradient-worker and
//! gradient-proxy.
//!
//! - `types`, `messages`, `constants` - wire payloads, rkyv codecs and limits.
//! - `session::{frame, handshake}` - direction-generic framing and the two
//!   role-symmetric handshake drivers (`as_peer`, `as_authority`) over an
//!   established `ProtoSocket`.
//! - `client::dial` / `server::{accept_axum, accept_tungstenite}` - how a
//!   `ProtoSocket` comes to exist.
//! - `traits` - the role traits every peer implements.
//! - `auth`, `transport`, `limiter` - pure token checks, NAR transport policy
//!   and connection caps.

pub mod auth;
pub mod build_output_metadata;
pub mod cached_path_info;
pub mod client;
pub mod constants;
pub mod limiter;
pub mod messages;
pub mod server;
pub mod session;
pub mod traits;
pub mod transport;
pub mod types;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use self::build_output_metadata::BuildOutputMetadata;
pub use self::cached_path_info::{CachedPathInfo, UploadTarget};
pub use self::limiter::{PerIpLimiter, ProtoLimiter};
pub use self::messages::{ClientMessage, PROTO_VERSION, ServerMessage};
pub use self::session::frame::{Frame, Inbound, WireMessage};
