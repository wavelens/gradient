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
    /// Whether to enable incoming worker and federation connections on `/proto`.
    #[arg(default_value = "true")]
    pub discoverable: bool,

    /// Whether to enable federation with other Gradient servers over `/proto`.
    #[arg(default_value = "false")]
    pub federate: bool,

    /// Maximum simultaneous `/proto` WebSocket connections.
    #[arg(id = "proto-max-connections", default_value = "256")]
    pub max_connections: usize,

    /// Seconds a connected worker can stay silent before counting as dead, with its jobs
    /// re-queued. Workers heartbeat every 10 seconds, giving the default a tolerance of twelve
    /// missed beats. The timeout is the only detection for a worker lost without a clean TCP
    /// close (OOM kill, frozen host, network partition). Set it to `0` to disable the watchdog.
    #[arg(default_value_t = 120)]
    pub worker_heartbeat_timeout_secs: u64,

    /// Whether unauthenticated clients can use `GET /cache/{cache}/proto` for public caches.
    /// Private caches always require an API key.
    #[arg(default_value = "true")]
    pub anonymous_cache_enable: bool,

    /// Maximum simultaneous anonymous `/cache/proto` connections per client IP.
    #[arg(default_value_t = 32)]
    pub anonymous_cache_max_connections_per_ip: usize,
}
