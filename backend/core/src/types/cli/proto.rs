/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ProtoArgs {
    /// Advertise HTTP/3 (QUIC) support to connecting clients.
    /// Enabling this does NOT change the backend transport - configure nginx
    /// with `listen 443 quic` and set the `Alt-Svc` header there.
    /// This flag is surfaced via `GET /api/v1/config` so clients can choose
    /// whether to attempt an HTTP/3 upgrade.
    #[arg(long, env = "GRADIENT_QUIC", default_value = "false")]
    pub quic: bool,

    /// Maximum number of simultaneous proto WebSocket connections.
    #[arg(long, env = "GRADIENT_MAX_PROTO_CONNECTIONS", default_value = "256")]
    pub max_proto_connections: usize,

    /// Accept incoming connections on `/proto` (workers and federated servers).
    /// Enabled by default - disable to reject all `/proto` connections.
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

    /// Maximum time the server will wait to open a NAR object stream
    /// (e.g. S3 GET) before giving up and emitting `NarUnavailable`. A stalled
    /// backend used to silently block the dispatch loop until the worker's
    /// 600 s receive-timeout fired; this caps it.
    #[arg(
        long,
        env = "GRADIENT_NAR_STORAGE_OPEN_TIMEOUT_SECS",
        default_value_t = 60
    )]
    pub nar_storage_open_timeout_secs: u64,

    /// Maximum time a single outbound `NarPush` chunk may sit in the writer
    /// queue waiting for the WebSocket sink to make progress. Hitting this
    /// timeout indicates a stalled peer / TCP back-pressure and aborts the
    /// transfer with `NarAbort`.
    #[arg(
        long,
        env = "GRADIENT_NAR_SEND_CHUNK_TIMEOUT_SECS",
        default_value_t = 30
    )]
    pub nar_send_chunk_timeout_secs: u64,

    /// Maximum number of NAR-serving tasks that may run concurrently per
    /// worker connection. Bounds memory and storage-backend fan-out when a
    /// worker requests many paths in a single batch.
    #[arg(long, env = "GRADIENT_MAX_CONCURRENT_NAR_SERVES", default_value_t = 8)]
    pub max_concurrent_nar_serves: usize,

    /// Maximum bytes a single proto session may hold in its inbound NAR
    /// upload buffers (open `NarPush` streams with no `is_final` yet). A
    /// rogue worker that opens many streams without finalizing them would
    /// otherwise pin unbounded RAM. (See issue #109.)
    #[arg(
        long,
        env = "GRADIENT_MAX_NAR_BUFFER_BYTES",
        default_value_t = 10 * 1024 * 1024 * 1024
    )]
    pub max_nar_buffer_bytes: usize,
}

impl Default for ProtoArgs {
    fn default() -> Self {
        Self {
            quic: false,
            max_proto_connections: 256,
            discoverable: true,
            federate_proto: false,
            global_stats_public: false,
            nar_storage_open_timeout_secs: 60,
            nar_send_chunk_timeout_secs: 30,
            max_concurrent_nar_serves: 8,
            max_nar_buffer_bytes: 10 * 1024 * 1024 * 1024,
        }
    }
}
