/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroUsize;
use std::time::Duration;

use clap::Args;
use gradient_derive::config_group;

pub const ASSIGNMENT_RECLAIM_GRACE: Duration = Duration::from_secs(600);

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
    pub max_connections: NonZeroUsize,

    /// Seconds a connected worker can stay silent before counting as dead, with its jobs
    /// re-queued. Workers heartbeat every 10 seconds, giving the default a tolerance of twelve
    /// missed beats. The timeout is the only detection for a worker lost without a clean TCP
    /// close (OOM kill, frozen host, network partition). Set it to `0` to disable the watchdog.
    #[arg(default_value_t = 120)]
    pub worker_heartbeat_timeout_secs: u64,

    /// Seconds the server keeps the open assignments of a disconnected worker for the worker to
    /// report them in `RunningJobs`. After a server restart this covers the open assignments of
    /// every worker. Assignments nobody reports within this time are re-queued and count as a
    /// lost worker. A worker with a `maxLifetime` gets at most its remaining lifetime. Set it to
    /// `0` to re-queue at once.
    #[arg(default_value_t = ASSIGNMENT_RECLAIM_GRACE.as_secs())]
    pub assignment_reclaim_grace_secs: u64,

    /// Whether unauthenticated clients can use `GET /cache/{cache}/proto` for public caches.
    /// Private caches always require an API key.
    #[arg(default_value = "true")]
    pub anonymous_cache_enable: bool,

    /// Maximum simultaneous anonymous `/cache/proto` connections per client IP.
    #[arg(default_value = "32")]
    pub anonymous_cache_max_connections_per_ip: NonZeroUsize,
}
