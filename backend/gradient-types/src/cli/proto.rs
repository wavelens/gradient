/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ProtoArgs {
    /// Accept incoming connections on `/proto` (workers and federated servers). It is enabled by
    /// default. Disable it to reject all `/proto` connections.
    #[arg(
        long = "proto-discoverable",
        env = "GRADIENT_PROTO_DISCOVERABLE",
        default_value = "true"
    )]
    pub discoverable: bool,

    /// Accept federated connections from other Gradient servers on `/proto`. `discoverable` must be
    /// enabled.
    #[arg(
        long = "proto-federate",
        env = "GRADIENT_PROTO_FEDERATE",
        default_value = "false"
    )]
    pub federate: bool,

    /// Maximum number of simultaneous proto WebSocket connections.
    #[arg(
        id = "proto-max-connections",
        long = "proto-max-connections",
        env = "GRADIENT_PROTO_MAX_CONNECTIONS",
        default_value = "256"
    )]
    pub max_connections: usize,

    /// Seconds a connected worker may go silent before the server is declaring it dead and
    /// re-queuing its in-flight jobs. The worker is sending a heartbeat every 10 s. The default of
    /// 120 s is tolerating twelve missed beats. The connection reader is stamping `last_seen` on
    /// every received frame. It is measuring the connection rather than handler duration. A server
    /// briefly stalled on slow DB acquires cannot falsely declare a healthy worker dead. Requeued
    /// in-flight builds would cost far more. This is the only detector for a worker dying without a
    /// clean TCP close, like a hard OOM-kill, a frozen host or a network partition. A graceful
    /// disconnect is handled immediately regardless. Set to 0 to disable the liveness watchdog.
    #[arg(
        long = "proto-worker-heartbeat-timeout-secs",
        env = "GRADIENT_PROTO_WORKER_HEARTBEAT_TIMEOUT_SECS",
        default_value_t = 120
    )]
    pub worker_heartbeat_timeout_secs: u64,

    /// Allow anonymous (unauthenticated) clients on `GET /cache/{cache}/proto` for public caches. A
    /// `false` value is rejecting anonymous handshakes with 403. Private caches always require an
    /// API key regardless of this flag.
    #[arg(
        long = "proto-anonymous-cache-enable",
        env = "GRADIENT_PROTO_ANONYMOUS_CACHE_ENABLE",
        default_value = "true"
    )]
    pub anonymous_cache_enable: bool,

    /// Maximum simultaneous anonymous `/proto` connections per client IP.
    #[arg(
        long = "proto-anonymous-cache-max-connections-per-ip",
        env = "GRADIENT_PROTO_ANONYMOUS_CACHE_MAX_CONNECTIONS_PER_IP",
        default_value_t = 32
    )]
    pub anonymous_cache_max_connections_per_ip: usize,
}

impl Default for ProtoArgs {
    fn default() -> Self {
        Self {
            discoverable: true,
            federate: false,
            max_connections: 256,
            worker_heartbeat_timeout_secs: 120,
            anonymous_cache_enable: true,
            anonymous_cache_max_connections_per_ip: 32,
        }
    }
}
