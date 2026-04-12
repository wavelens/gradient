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
    /// Format: one `peer_id:token` pair per line (newline-separated).
    /// Use `*:token` to respond with `token` for any peer UUID the server challenges.
    /// Each named peer is an org, cache, or proxy UUID.
    /// Mutually exclusive with `--peers-file`.
    #[arg(long, env = "GRADIENT_WORKER_PEERS")]
    pub peers: Option<String>,

    /// Path to a file whose contents are peer-to-token pairs, one per line
    /// (same format as `--peers`). Takes precedence over `--peers`.
    #[arg(long, env = "GRADIENT_WORKER_PEERS_FILE")]
    pub peers_file: Option<String>,

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
    /// Parse peer-to-token pairs from `--peers-file` (preferred) or `--peers`.
    /// Returns an empty vec when neither is set (open/discoverable mode).
    ///
    /// Format: one `peer_id:token` entry per line. The special peer ID `*`
    /// matches any UUID the server challenges — callers should expand it using
    /// [`Self::resolve_tokens_for_challenge`].
    pub fn peer_tokens(&self) -> Vec<(String, String)> {
        let raw = if let Some(path) = &self.peers_file {
            match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(path, error = %e, "failed to read peers file; connecting in open mode");
                    return vec![];
                }
            }
        } else if let Some(s) = &self.peers {
            s.clone()
        } else {
            return vec![];
        };

        raw.lines()
            .filter_map(|entry| {
                let entry = entry.trim();
                if entry.is_empty() || entry.starts_with('#') {
                    return None;
                }
                let mut parts = entry.splitn(2, ':');
                let peer_id = parts.next()?.trim().to_owned();
                let token = parts.next()?.trim().to_owned();
                if peer_id.is_empty() || token.is_empty() {
                    return None;
                }
                // Tokens must be the base64 encoding of 48 random bytes
                // (64 characters), as produced by `openssl rand -base64 48`
                // or the worker registration API.
                if token.len() < 64 {
                    tracing::warn!(
                        peer_id,
                        token_len = token.len(),
                        "token is too short (expected 64 base64 chars / 48 bytes); skipping entry"
                    );
                    return None;
                }
                Some((peer_id, token))
            })
            .collect()
    }

    /// Given the list of peer UUIDs the server challenged us about, build the
    /// `(peer_id, token)` pairs to include in `AuthResponse`.
    ///
    /// A wildcard entry (`*:token`) expands to a response for every challenged
    /// peer that is not already covered by an explicit entry.
    pub fn resolve_tokens_for_challenge(
        peer_tokens: &[(String, String)],
        challenged: &[String],
    ) -> Vec<(String, String)> {
        let wildcard_token: Option<&str> = peer_tokens
            .iter()
            .find(|(id, _)| id == "*")
            .map(|(_, t)| t.as_str());

        let mut result: Vec<(String, String)> = peer_tokens
            .iter()
            .filter(|(id, _)| id != "*" && challenged.contains(id))
            .cloned()
            .collect();

        if let Some(token) = wildcard_token {
            let covered: std::collections::HashSet<String> =
                result.iter().map(|(id, _)| id.clone()).collect();
            let extras: Vec<(String, String)> = challenged
                .iter()
                .filter(|pid| !covered.contains(*pid))
                .map(|pid| (pid.clone(), token.to_owned()))
                .collect();
            result.extend(extras);
        }

        result
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
