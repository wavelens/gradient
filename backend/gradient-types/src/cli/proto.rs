/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("proto")]
#[derive(Args, Debug, Clone)]
pub struct ProtoArgs {
    /// Accept incoming connections on `/proto` (workers and federated servers). It is enabled by
    /// default. Disable it to reject all `/proto` connections.
    #[arg(default_value = "true")]
    pub discoverable: bool,

    /// Accept federated connections from other Gradient servers on `/proto`. `discoverable` must be
    /// enabled.
    #[arg(default_value = "false")]
    pub federate: bool,

    /// Maximum number of simultaneous proto WebSocket connections.
    #[arg(id = "proto-max-connections", default_value = "256")]
    pub max_connections: usize,

    /// Seconds a connected worker may go silent before the server is declaring it dead and
    /// re-queuing its in-flight jobs. The worker is sending a heartbeat every 10 s. The default of
    /// 120 s is tolerating twelve missed beats. The connection reader is stamping `last_seen` on
    /// every received frame. It is measuring the connection rather than handler duration. A server
    /// briefly stalled on slow DB acquires cannot falsely declare a healthy worker dead. Requeued
    /// in-flight builds would cost far more. This is the only detector for a worker dying without a
    /// clean TCP close, like a hard OOM-kill, a frozen host or a network partition. A graceful
    /// disconnect is handled immediately regardless. Set to 0 to disable the liveness watchdog.
    #[arg(default_value_t = 120)]
    pub worker_heartbeat_timeout_secs: u64,

    /// Allow anonymous (unauthenticated) clients on `GET /cache/{cache}/proto` for public caches. A
    /// `false` value is rejecting anonymous handshakes with 403. Private caches always require an
    /// API key regardless of this flag.
    #[arg(default_value = "true")]
    pub anonymous_cache_enable: bool,

    /// Maximum simultaneous anonymous `/proto` connections per client IP.
    #[arg(default_value_t = 32)]
    pub anonymous_cache_max_connections_per_ip: usize,
}
