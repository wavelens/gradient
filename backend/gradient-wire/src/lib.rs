/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod auth;
pub mod build_output_metadata;
pub mod cached_path_info;
pub mod client;
pub mod codec;
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
pub use self::cached_path_info::CachedPathInfo;
pub use self::limiter::{PerIpLimiter, ProtoLimiter};
pub use self::messages::{ClientMessage, PROTO_VERSION, ServerMessage};
pub use self::session::frame::{Frame, Inbound, WireMessage};
