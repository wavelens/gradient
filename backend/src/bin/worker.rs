/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "gradient-worker", about = "Gradient build worker")]
struct Cli {
    /// URL of the Gradient server WebSocket protocol endpoint.
    #[arg(long, env = "GRADIENT_WORKER_SERVER_URL")]
    server_url: String,

    /// File containing the API key used to authenticate with the server.
    /// Not required for cache-only (public) connections.
    #[arg(long, env = "GRADIENT_WORKER_TOKEN_FILE")]
    token_file: Option<String>,

    /// Re-exec as a Nix evaluator subprocess (internal, do not set manually).
    #[arg(long, env = "GRADIENT_EVAL_WORKER", hide = true)]
    eval_worker: bool,

    /// Number of parallel Nix evaluator subprocesses.
    /// Only used when --capability-eval is enabled.
    #[arg(long, env = "GRADIENT_WORKER_EVAL_WORKERS", default_value_t = 1)]
    eval_workers: usize,

    #[arg(long, env = "GRADIENT_LOG_LEVEL", default_value = "info")]
    log_level: String,

    /// Log level for the evaluator. Overrides log_level for eval tasks.
    #[arg(long, env = "GRADIENT_EVAL_LOG_LEVEL")]
    eval_log_level: Option<String>,

    /// Log level for the builder. Overrides log_level for build tasks.
    #[arg(long, env = "GRADIENT_BUILD_LOG_LEVEL")]
    build_log_level: Option<String>,

    /// Log level for the protocol layer. Overrides log_level for proto.
    #[arg(long, env = "GRADIENT_PROTO_LOG_LEVEL")]
    proto_log_level: Option<String>,

    /// Maximum number of simultaneous proto WebSocket connections.
    #[arg(long, env = "GRADIENT_MAX_PROTO_CONNECTIONS", default_value = "16")]
    max_proto_connections: usize,

    /// Listen for incoming connections (discoverable by servers).
    /// Must be enabled for federate capability to work, but can be enabled on its own.
    #[arg(long, env = "GRADIENT_WORKER_DISCOVERABLE", default_value = "false")]
    discoverable: bool,

    // ── Capabilities ──────────────────────────────────────────────────────────
    /// Support federation — relay work and NAR traffic between workers and servers.
    /// Requires discoverable to be enabled.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_FEDERATE", default_value = "false")]
    capability_federate: bool,

    /// Prefetch flake inputs and sources on behalf of the server.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_FETCH", default_value = "false")]
    capability_fetch: bool,

    /// Run Nix flake evaluations.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_EVAL", default_value = "false")]
    capability_eval: bool,

    /// Execute Nix store builds.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_BUILD", default_value = "false")]
    capability_build: bool,

    /// Sign and upload store paths.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_SIGN", default_value = "false")]
    capability_sign: bool,
}

fn main() {
    let _cli = Cli::parse();

    // TODO:
    // - if cli.eval_worker { builder::evaluator::run_eval_worker(); return; }
    // - connect to cli.server_url via WebSocket
    // - send InitConnection with GradientCapabilities built from cli.capability_* flags
    // - main dispatch loop
}
