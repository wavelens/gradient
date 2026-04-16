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
    #[arg(
        long,
        env = "GRADIENT_WORKER_DATA_DIR",
        default_value = "/var/lib/gradient-worker"
    )]
    pub data_dir: String,

    /// Override the worker's persistent UUID. When set, this value is used as
    /// the worker identity instead of the UUID stored in `{data_dir}/worker-id`.
    /// Useful for declarative deployments where the ID must be known before the
    /// worker first runs. Must be a valid UUID.
    #[arg(long, env = "GRADIENT_WORKER_ID")]
    pub worker_id: Option<String>,

    /// Path to the `nix` binary. Defaults to `nix` (resolved via `PATH`).
    #[arg(long, env = "GRADIENT_BINPATH_NIX", default_value = "nix")]
    pub binpath_nix: String,

    /// Path to the `ssh` binary. Used as `GIT_SSH_COMMAND` when nix fetches
    /// private flake inputs. Defaults to `ssh` (resolved via `PATH`).
    #[arg(long, env = "GRADIENT_BINPATH_SSH", default_value = "ssh")]
    pub binpath_ssh: String,

    /// Re-exec as a Nix evaluator subprocess (internal — do not set manually).
    #[arg(long, env = "GRADIENT_EVAL_WORKER", hide = true)]
    pub eval_worker: bool,

    /// Number of parallel Nix evaluator subprocesses.
    /// Only effective when `--capability-eval` is enabled.
    #[arg(long, env = "GRADIENT_WORKER_EVAL_WORKERS", default_value_t = 1)]
    pub eval_workers: usize,

    /// Recycle an eval-worker subprocess after serving this many list/resolve/attr_names
    /// calls. Nix's Boehm GC never releases memory back to the OS; recycling is the
    /// only way to bound memory usage. Set to 0 to disable recycling.
    #[arg(long, env = "GRADIENT_MAX_EVALUATIONS_PER_WORKER", default_value_t = 1)]
    pub max_evals_per_worker: usize,

    /// Maximum number of simultaneous evaluations.
    /// Defaults to `eval_workers` (one eval job per evaluator subprocess).
    #[arg(
        long,
        env = "GRADIENT_MAX_CONCURRENT_EVALUATIONS",
        default_value_t = 1
    )]
    pub max_concurrent_evaluations: u32,

    /// Maximum number of simultaneous builds.
    #[arg(
        long,
        env = "GRADIENT_MAX_CONCURRENT_BUILDS",
        default_value_t = 1
    )]
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

    /// IP address on which to listen for incoming server connections when discoverable.
    #[arg(long, env = "GRADIENT_WORKER_LISTEN_ADDR", default_value = "127.0.0.1")]
    pub listen_addr: String,

    /// Port on which to listen for incoming server connections when discoverable.
    #[arg(long, env = "GRADIENT_WORKER_PORT", default_value_t = 3100)]
    pub port: u16,

    // ── Capabilities ──────────────────────────────────────────────────────────
    /// Relay work and NAR traffic between workers and servers (federation).
    /// Requires `--discoverable`.
    #[arg(
        long,
        env = "GRADIENT_WORKER_CAPABILITY_FEDERATE",
        default_value = "false"
    )]
    pub capability_federate: bool,

    /// Prefetch flake inputs and sources.
    #[arg(
        long,
        env = "GRADIENT_WORKER_CAPABILITY_FETCH",
        default_value = "false"
    )]
    pub capability_fetch: bool,

    /// Run Nix flake evaluations.
    #[arg(long, env = "GRADIENT_WORKER_CAPABILITY_EVAL", default_value = "false")]
    pub capability_eval: bool,

    /// Execute Nix store builds locally.
    #[arg(
        long,
        env = "GRADIENT_WORKER_CAPABILITY_BUILD",
        default_value = "false"
    )]
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
                // Tokens must be exactly the base64 encoding of 48 random bytes
                // (64 characters), as produced by `openssl rand -base64 48`
                // or the worker registration API.
                if token.len() != 64 {
                    tracing::warn!(
                        peer_id,
                        token_len = token.len(),
                        "token must be exactly 64 base64 chars (48 bytes); skipping entry"
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: build a minimal WorkerConfig with `peers` set ────────────────

    fn config_with_peers(peers: &str) -> WorkerConfig {
        WorkerConfig {
            server_url: String::new(),
            peers: Some(peers.to_owned()),
            peers_file: None,
            data_dir: String::new(),
            worker_id: None,
            binpath_nix: "nix".to_owned(),
            binpath_ssh: "ssh".to_owned(),
            eval_worker: false,
            eval_workers: 1,
            max_evals_per_worker: 1,
            max_concurrent_evaluations: 1,
            max_concurrent_builds: 1,
            log_level: "info".to_owned(),
            eval_log_level: None,
            build_log_level: None,
            proto_log_level: None,
            listen_addr: "127.0.0.1".to_owned(),
            discoverable: false,
            port: 3100,
            capability_federate: false,
            capability_fetch: false,
            capability_eval: false,
            capability_build: false,
            capability_sign: false,
        }
    }

    /// 64 `x` characters — the only accepted token length.
    const TOK64: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    /// 63 `x` characters — one too short.
    const TOK63: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    /// 65 `x` characters — one too long.
    const TOK65: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

    // ── peer_tokens() ─────────────────────────────────────────────────────────

    #[test]
    fn peer_tokens_from_inline_string() {
        let cfg = config_with_peers(&format!("peer1:{TOK64}\npeer2:{TOK64}"));
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0], ("peer1".to_owned(), TOK64.to_owned()));
        assert_eq!(tokens[1], ("peer2".to_owned(), TOK64.to_owned()));
    }

    #[test]
    fn peer_tokens_skips_blank_lines_and_comments() {
        let input = format!("\n# this is a comment\npeer:{TOK64}\n\n");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer");
    }

    #[test]
    fn peer_tokens_skips_short_tokens() {
        let input = format!("peer-short:{TOK63}\npeer-ok:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-ok");
    }

    #[test]
    fn peer_tokens_skips_long_tokens() {
        let input = format!("peer-long:{TOK65}\npeer-ok:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-ok");
    }

    #[test]
    fn peer_tokens_empty_when_neither_set() {
        let cfg = WorkerConfig {
            server_url: String::new(),
            peers: None,
            peers_file: None,
            data_dir: String::new(),
            worker_id: None,
            binpath_nix: "nix".to_owned(),
            binpath_ssh: "ssh".to_owned(),
            eval_worker: false,
            eval_workers: 1,
            max_evals_per_worker: 1,
            max_concurrent_evaluations: 1,
            max_concurrent_builds: 1,
            log_level: "info".to_owned(),
            eval_log_level: None,
            build_log_level: None,
            proto_log_level: None,
            listen_addr: "127.0.0.1".to_owned(),
            discoverable: false,
            port: 3100,
            capability_federate: false,
            capability_fetch: false,
            capability_eval: false,
            capability_build: false,
            capability_sign: false,
        };
        assert!(cfg.peer_tokens().is_empty());
    }

    #[test]
    fn peer_tokens_skips_empty_peer_or_token() {
        // ":token" → peer_id is empty
        // "peer:" → token is empty
        // "nocolon" → no separator → skipped
        let input = format!(":tok64ok\npeer:\nnocolon\npeer-valid:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-valid");
    }

    #[test]
    fn peer_tokens_preserves_wildcard() {
        let input = format!("*:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], ("*".to_owned(), TOK64.to_owned()));
    }

    #[test]
    fn peer_tokens_from_file() {
        let path = std::env::temp_dir()
            .join(format!("gradient-test-peers-{}", std::process::id()))
            .to_str()
            .unwrap()
            .to_owned();
        std::fs::write(&path, format!("peer-file:{TOK64}")).unwrap();

        let cfg = WorkerConfig {
            server_url: String::new(),
            peers: Some(format!("peer-inline:{TOK64}")), // should be ignored
            peers_file: Some(path.clone()),
            data_dir: String::new(),
            worker_id: None,
            binpath_nix: "nix".to_owned(),
            binpath_ssh: "ssh".to_owned(),
            eval_worker: false,
            eval_workers: 1,
            max_evals_per_worker: 1,
            max_concurrent_evaluations: 1,
            max_concurrent_builds: 1,
            log_level: "info".to_owned(),
            eval_log_level: None,
            build_log_level: None,
            proto_log_level: None,
            listen_addr: "127.0.0.1".to_owned(),
            discoverable: false,
            port: 3100,
            capability_federate: false,
            capability_fetch: false,
            capability_eval: false,
            capability_build: false,
            capability_sign: false,
        };
        let tokens = cfg.peer_tokens();
        let _ = std::fs::remove_file(&path);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-file");
    }

    // ── resolve_tokens_for_challenge() ────────────────────────────────────────

    #[test]
    fn resolve_tokens_explicit_only() {
        let tokens = vec![
            ("peer-a".to_owned(), "tok-a".to_owned()),
            ("peer-c".to_owned(), "tok-c".to_owned()),
        ];
        let challenged = vec!["peer-a".to_owned(), "peer-b".to_owned()];
        let result = WorkerConfig::resolve_tokens_for_challenge(&tokens, &challenged);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("peer-a".to_owned(), "tok-a".to_owned()));
    }

    #[test]
    fn resolve_tokens_wildcard_fills_gaps() {
        let tokens = vec![
            ("*".to_owned(), "wild".to_owned()),
            ("peer-a".to_owned(), "tok-a".to_owned()),
        ];
        let challenged = vec![
            "peer-a".to_owned(),
            "peer-b".to_owned(),
            "peer-c".to_owned(),
        ];
        let result = WorkerConfig::resolve_tokens_for_challenge(&tokens, &challenged);
        assert_eq!(result.len(), 3);

        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map["peer-a"], "tok-a");
        assert_eq!(map["peer-b"], "wild");
        assert_eq!(map["peer-c"], "wild");
    }

    #[test]
    fn resolve_tokens_wildcard_only() {
        let tokens = vec![("*".to_owned(), "wild".to_owned())];
        let challenged = vec!["p1".to_owned(), "p2".to_owned(), "p3".to_owned()];
        let result = WorkerConfig::resolve_tokens_for_challenge(&tokens, &challenged);
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|(_, t)| t == "wild"));
    }

    #[test]
    fn resolve_tokens_empty_when_no_match() {
        let tokens = vec![("peer-x".to_owned(), "tok".to_owned())];
        let challenged = vec!["peer-y".to_owned()];
        let result = WorkerConfig::resolve_tokens_for_challenge(&tokens, &challenged);
        assert!(result.is_empty());
    }
}
