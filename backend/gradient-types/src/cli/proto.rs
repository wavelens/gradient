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

    /// Allow anonymous (unauthenticated) clients on `GET /cache/{cache}/proto`
    /// for public caches. When `false`, anonymous handshakes are rejected with
    /// 403; private caches always require an API key regardless of this flag.
    #[arg(
        long,
        env = "GRADIENT_PROTO_ALLOW_ANONYMOUS_CACHE",
        default_value = "true"
    )]
    pub allow_anonymous_cache: bool,

    /// Maximum simultaneous anonymous `/proto` connections per client IP.
    #[arg(
        long,
        env = "GRADIENT_PROTO_ANON_MAX_CONNECTIONS_PER_IP",
        default_value_t = 32
    )]
    pub anon_max_connections_per_ip: usize,

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

    /// Maximum total bytes the server may hold across open `*.partial` NAR
    /// upload files (un-finalized `NarPush` streams staged under
    /// `<base_path>/nar-partial`). A rogue worker opening many streams without
    /// finalizing them would otherwise fill the disk; overflow aborts the
    /// offending path with `NarAbort`. (See issue #109.)
    #[arg(
        long,
        env = "GRADIENT_MAX_NAR_BUFFER_BYTES",
        default_value_t = 10 * 1024 * 1024 * 1024
    )]
    pub max_nar_buffer_bytes: usize,

    /// TTL in seconds for partially-received NAR uploads (`*.partial`) staged
    /// under `<base_path>/nar-partial`. A periodic sweep deletes partials whose
    /// last write is older than this so an abandoned resume can't pin disk
    /// forever. Default 86400 (24 h). Set to 0 to disable the sweep.
    #[arg(long, env = "GRADIENT_NAR_PARTIAL_TTL_SECS", default_value_t = 86400)]
    pub nar_partial_ttl_secs: u64,

    /// Seconds a connected worker may go silent before the server declares it
    /// dead and re-queues its in-flight jobs. The worker heartbeats every 10 s,
    /// so the default 30 s tolerates three missed beats. This is the only
    /// detector for a worker that dies without a clean TCP close (hard OOM-kill,
    /// frozen host, network partition); a graceful disconnect is handled
    /// immediately regardless. Set to 0 to disable the liveness watchdog.
    #[arg(
        long,
        env = "GRADIENT_WORKER_HEARTBEAT_TIMEOUT_SECS",
        default_value_t = 30
    )]
    pub worker_heartbeat_timeout_secs: u64,

    /// Maximum simultaneous outbound upstream narinfo requests across the whole
    /// server (eval-time substitutability probes and worker cache-query probes
    /// share this pool), so a huge evaluation never fans out one request per
    /// derivation times every upstream at once.
    #[arg(
        long,
        env = "GRADIENT_UPSTREAM_QUERY_CONCURRENCY",
        default_value_t = 32
    )]
    pub upstream_query_concurrency: usize,
}

impl Default for ProtoArgs {
    fn default() -> Self {
        Self {
            quic: false,
            max_proto_connections: 256,
            discoverable: true,
            federate_proto: false,
            global_stats_public: false,
            allow_anonymous_cache: true,
            anon_max_connections_per_ip: 32,
            nar_storage_open_timeout_secs: 60,
            nar_send_chunk_timeout_secs: 30,
            max_concurrent_nar_serves: 8,
            max_nar_buffer_bytes: 10 * 1024 * 1024 * 1024,
            nar_partial_ttl_secs: 86400,
            worker_heartbeat_timeout_secs: 30,
            upstream_query_concurrency: 32,
        }
    }
}
