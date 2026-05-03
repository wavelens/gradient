/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ProtoArgs {
    /// Advertise HTTP/3 (QUIC) support to connecting clients.
    /// Enabling this does NOT change the backend transport — configure nginx
    /// with `listen 443 quic` and set the `Alt-Svc` header there.
    /// This flag is surfaced via `GET /api/v1/config` so clients can choose
    /// whether to attempt an HTTP/3 upgrade.
    #[arg(long, env = "GRADIENT_QUIC", default_value = "false")]
    pub quic: bool,

    /// Maximum number of simultaneous proto WebSocket connections.
    #[arg(long, env = "GRADIENT_MAX_PROTO_CONNECTIONS", default_value = "256")]
    pub max_proto_connections: usize,

    /// Accept incoming connections on `/proto` (workers and federated servers).
    /// Enabled by default — disable to reject all `/proto` connections.
    #[arg(long, env = "GRADIENT_DISCOVERABLE", default_value = "true")]
    pub discoverable: bool,

    /// Accept federated connections from other Gradient servers on `/proto`.
    /// Requires `discoverable` to be enabled.
    #[arg(long, env = "GRADIENT_FEDERATE_PROTO", default_value = "false")]
    pub federate_proto: bool,

    /// Expose `GET /api/v1/workers` and worker stats without authentication.
    /// When `false` (default), only superusers can access those endpoints.
    #[arg(long, env = "GRADIENT_GLOBAL_STATS_PUBLIC", default_value = "false")]
    pub global_stats_public: bool,
}

impl Default for ProtoArgs {
    fn default() -> Self {
        Self {
            quic: false,
            max_proto_connections: 256,
            discoverable: true,
            federate_proto: false,
            global_stats_public: false,
        }
    }
}
