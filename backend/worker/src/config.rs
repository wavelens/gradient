/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Parser;
use gradient_core::types::proto::GradientCapabilities;

/// CLI arguments and environment variables for `gradient-worker`.
#[derive(Parser, Debug, Clone)]
#[command(name = "gradient-worker", about = "Gradient build worker")]
pub struct WorkerConfig {
    /// WebSocket URL of the Gradient server's `/proto` endpoint.
    /// Example: `wss://gradient.example.com/proto`
    #[arg(long, env = "GRADIENT_WORKER_SERVER_URL")]
    pub server_url: String,

    /// Peer-to-token mappings for challenge-response authentication.
    /// Format: `peer_id1:token1,peer_id2:token2` (comma-separated pairs).
    /// Each peer can be an org, cache, or proxy UUID.
    #[arg(long, env = "GRADIENT_WORKER_PEERS")]
    pub peers: Option<String>,

    /// Directory for persistent worker state (worker ID file, etc.).
    /// Defaults to `/var/lib/gradient-worker`. Must be writable.
    #[arg(long, env = "GRADIENT_WORKER_DATA_DIR", default_value = "/var/lib/gradient-worker")]
    pub data_dir: String,

    /// Re-exec as a Nix evaluator subprocess (internal — do not set manually).
    #[arg(long, env = "GRADIENT_EVAL_WORKER", hide = true)]
    pub eval_worker: bool,

    /// Number of parallel Nix evaluator subprocesses.
    /// Only effective when `--capability-eval` is enabled.
    #[arg(long, env = "GRADIENT_WORKER_EVAL_WORKERS", default_value_t = 1)]
    pub eval_workers: usize,

    /// Maximum number of simultaneous builds.
    #[arg(long, env = "GRADIENT_WORKER_MAX_CONCURRENT_BUILDS", default_value_t = 1)]
    pub max_concurrent_builds: u32,

    // ── Logging ───────────────────────────────────────────────────────────────

    #[arg(long, env = "GRADIENT_LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    #[arg(long, env = "GRADIENT_EVAL_LOG_LEVEL")]
    pub eval_log_level: Option<String>,

    #[arg(long, env = "GRADIENT_BUILD_LOG_LEVEL")]
    pub build_log_level: Option<String>,

    #[arg(long, env = "GRADIENT_PROTO_LOG_LEVEL")]
    pub proto_log_level: Option<String>,

    // ── Network ───────────────────────────────────────────────────────────────

    /// Accept incoming `/proto` connections from the server (reverse-proxy mode).
    #[arg(long, env = "GRADIENT_WORKER_DISCOVERABLE", default_value = "false")]
    pub discoverable: bool,

    // ── Capabilities ──────────────────────────────────────────────────────────

    /// Relay work and NAR traffic between workers and servers (federation).
    /// Requires `--discoverable`.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_FEDERATE", default_value = "false")]
    pub capability_federate: bool,

    /// Prefetch flake inputs and sources.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_FETCH", default_value = "false")]
    pub capability_fetch: bool,

    /// Run Nix flake evaluations.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_EVAL", default_value = "false")]
    pub capability_eval: bool,

    /// Execute Nix store builds locally.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_BUILD", default_value = "false")]
    pub capability_build: bool,

    /// Sign store paths and upload signatures.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_SIGN", default_value = "false")]
    pub capability_sign: bool,
}

impl WorkerConfig {
    /// Parse `GRADIENT_WORKER_PEERS` into `(peer_id, token)` pairs.
    pub fn peer_tokens(&self) -> Vec<(String, String)> {
        let Some(peers) = &self.peers else { return vec![] };
        peers
            .split(',')
            .filter_map(|entry| {
                let mut parts = entry.splitn(2, ':');
                let peer_id = parts.next()?.trim().to_owned();
                let token = parts.next()?.trim().to_owned();
                if peer_id.is_empty() || token.is_empty() {
                    return None;
                }
                Some((peer_id, token))
            })
            .collect()
    }

    /// Build the `GradientCapabilities` struct from the CLI flags.
    pub fn capabilities(&self) -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: self.capability_federate,
            fetch: self.capability_fetch,
            eval: self.capability_eval,
            build: self.capability_build,
            sign: self.capability_sign,
            cache: false, // workers never serve as cache
        }
    }
}
