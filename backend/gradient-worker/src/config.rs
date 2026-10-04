/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::{Args, Parser};
use gradient_wire::types::GradientCapabilities;

/// The pool is capped at 16 workers because each one may hold `eval.max_rss` resident.
fn default_fork_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(16))
        .unwrap_or(4)
}

/// CLI arguments and environment variables for `gradient-worker`.
#[derive(Parser, Debug, Clone)]
#[command(name = "gradient-worker", about = "Gradient build worker")]
pub struct WorkerConfig {
    /// WebSocket URL of the Gradient server's `/proto` endpoint.
    /// An example is `wss://gradient.example.com/proto`.
    #[arg(long = "server-url", env = "GRADIENT_WORKER_SERVER_URL")]
    pub server_url: String,

    /// Peer-to-token mappings for challenge-response authentication.
    /// Each line is holding one `peer_id:token` pair.
    /// A `*:token` line is answering every challenged peer UUID with `token`.
    /// Each named peer is a project, cache, or proxy UUID.
    /// This option is mutually exclusive with `--peers-file`.
    #[arg(long = "peers", env = "GRADIENT_WORKER_PEERS")]
    pub peers: Option<String>,

    /// Path to a file with peer-to-token pairs in the `--peers` format.
    /// This file is taking precedence over `--peers`.
    #[arg(long = "peers-file", env = "GRADIENT_WORKER_PEERS_FILE")]
    pub peers_file: Option<String>,

    /// Directory for persistent worker state like the worker ID file.
    /// The directory must be writable.
    #[arg(
        long = "base-dir",
        env = "GRADIENT_WORKER_BASE_DIR",
        default_value = "/var/lib/gradient-worker"
    )]
    pub base_dir: String,

    /// Worker UUID used in place of the one stored in `{base_dir}/worker-id`.
    /// Declarative deployments can know the ID before the first worker start.
    /// The value must be a valid UUID.
    #[arg(long = "id", env = "GRADIENT_WORKER_ID")]
    pub id: Option<String>,

    /// Locality label advertised to the scheduler.
    /// Cluster jobs asking for one zone are placing all members on workers sharing a label.
    /// Workers without a label are forming a zone of their own.
    #[arg(long = "zone", env = "GRADIENT_WORKER_ZONE")]
    pub zone: Option<String>,

    /// Address other members of a cluster job are reaching this worker at.
    /// The cluster roster is carrying it verbatim.
    #[arg(long = "endpoint", env = "GRADIENT_WORKER_ENDPOINT")]
    pub endpoint: Option<String>,

    /// Path to the `nix` binary, resolved via `PATH` by default.
    #[arg(
        long = "nix-bin",
        env = "GRADIENT_WORKER_NIX_BIN",
        default_value = "nix"
    )]
    pub nix_bin: String,

    /// Path to the `ssh` binary, resolved via `PATH` by default.
    /// Nix is using it as `GIT_SSH_COMMAND` for private flake inputs.
    #[arg(
        long = "ssh-bin",
        env = "GRADIENT_WORKER_SSH_BIN",
        default_value = "ssh"
    )]
    pub ssh_bin: String,

    /// Directory for worker-held indirect GC roots.
    /// One symlink per active build is pinning its inputs and outputs while it is running.
    /// An empty string is disabling GC root pinning.
    /// A concurrent `nix-collect-garbage` can then race the build.
    #[arg(
        long = "gcroots-dir",
        env = "GRADIENT_WORKER_GCROOTS_DIR",
        default_value = "/nix/var/nix/gcroots/gradient"
    )]
    pub gcroots_dir: String,

    /// Seconds a SIGINT/SIGTERM drain is waiting for in-flight jobs.
    /// The worker is stopping new work at once, then finishing, reporting and exiting.
    /// Jobs still running at the deadline are aborted and re-queued by the server.
    /// A value of 0 is waiting indefinitely.
    /// The unit's `TimeoutStopSec` must stay above this value.
    #[arg(
        long = "drain-timeout-secs",
        env = "GRADIENT_WORKER_DRAIN_TIMEOUT_SECS",
        default_value_t = 600
    )]
    pub drain_timeout_secs: u64,

    /// Accept incoming `/proto` connections from the server (reverse-proxy mode).
    #[arg(
        long = "discoverable",
        env = "GRADIENT_WORKER_DISCOVERABLE",
        default_value = "false"
    )]
    pub discoverable: bool,

    /// IP address for incoming server connections in discoverable mode.
    #[arg(
        long = "listen-addr",
        env = "GRADIENT_WORKER_LISTEN_ADDR",
        default_value = "127.0.0.1"
    )]
    pub listen_addr: String,

    /// Port for incoming server connections in discoverable mode.
    #[arg(long = "port", env = "GRADIENT_WORKER_PORT", default_value_t = 3100)]
    pub port: u16,

    #[arg(
        long = "accepted-server-tokens-file",
        env = "GRADIENT_WORKER_ACCEPTED_SERVER_TOKENS_FILE",
        help = "File of `peer_id:hash` lines a dialing server's tokens must match in discoverable mode; without it every server is accepted"
    )]
    pub accepted_server_tokens_file: Option<String>,

    /// Re-exec as a Nix evaluator subprocess (internal - do not set manually).
    #[arg(
        long = "eval-subprocess",
        env = "GRADIENT_WORKER_EVAL_SUBPROCESS",
        hide = true
    )]
    pub eval_subprocess: bool,

    /// Drive one eval subprocess over the production rkyv transport from a JSONL request file.
    /// This internal harness is serving the NixOS VM integration test.
    #[arg(long = "eval-driver", env = "GRADIENT_WORKER_EVAL_DRIVER", hide = true)]
    pub eval_driver: Option<String>,

    #[command(flatten)]
    pub capabilities: CapabilitiesArgs,
    #[command(flatten)]
    pub system: SystemArgs,
    #[command(flatten)]
    pub nix_daemon: NixDaemonArgs,
    #[command(flatten)]
    pub eval: EvalArgs,
    #[command(flatten)]
    pub build: BuildArgs,
    #[command(flatten)]
    pub nar: NarArgs,
    #[command(flatten)]
    pub log: LogArgs,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            server_url: String::new(),
            peers: None,
            peers_file: None,
            base_dir: "/var/lib/gradient-worker".to_owned(),
            id: None,
            zone: None,
            endpoint: None,
            nix_bin: "nix".to_owned(),
            ssh_bin: "ssh".to_owned(),
            gcroots_dir: "/nix/var/nix/gcroots/gradient".to_owned(),
            drain_timeout_secs: 600,
            discoverable: false,
            listen_addr: "127.0.0.1".to_owned(),
            port: 3100,
            accepted_server_tokens_file: None,
            eval_subprocess: false,
            eval_driver: None,
            capabilities: CapabilitiesArgs::default(),
            system: SystemArgs::default(),
            nix_daemon: NixDaemonArgs::default(),
            eval: EvalArgs::default(),
            build: BuildArgs::default(),
            nar: NarArgs::default(),
            log: LogArgs::default(),
        }
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct CapabilitiesArgs {
    /// Forward work and NAR traffic between workers and servers (federation).
    /// `--discoverable` is required.
    #[arg(
        long = "capabilities-federate",
        env = "GRADIENT_WORKER_CAPABILITIES_FEDERATE",
        default_value = "false"
    )]
    pub federate: bool,

    /// Prefetch flake inputs and sources.
    #[arg(
        long = "capabilities-fetch",
        env = "GRADIENT_WORKER_CAPABILITIES_FETCH",
        default_value = "false"
    )]
    pub fetch: bool,

    /// Run Nix flake evaluations.
    #[arg(
        long = "capabilities-eval",
        env = "GRADIENT_WORKER_CAPABILITIES_EVAL",
        default_value = "false"
    )]
    pub eval: bool,

    /// Execute Nix store builds locally.
    #[arg(
        long = "capabilities-build",
        env = "GRADIENT_WORKER_CAPABILITIES_BUILD",
        default_value = "false"
    )]
    pub build: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct SystemArgs {
    /// Comma-separated Nix system strings this worker can build for.
    /// The default is the host system, for example `x86_64-linux`.
    /// A binfmt host can add emulated targets like `x86_64-linux,aarch64-linux`.
    /// The server's dispatcher is gating build assignment on this list.
    #[arg(
        long = "system-architectures",
        env = "GRADIENT_WORKER_SYSTEM_ARCHITECTURES",
        value_delimiter = ','
    )]
    pub architectures: Option<Vec<String>>,

    /// Comma-separated Nix system features this worker is advertising, like `kvm,big-parallel`.
    /// Builds requiring other features are never assigned to this worker.
    /// The default is the daemon's `nix config show system-features` set.
    /// That set is including CPU-derived `gccarch-*` levels.
    #[arg(
        long = "system-features",
        env = "GRADIENT_WORKER_SYSTEM_FEATURES",
        value_delimiter = ','
    )]
    pub features: Option<Vec<String>>,

    /// Single-core speed score advertised to the scheduler.
    /// The worker is running a deterministic micro-benchmark at startup without it.
    #[arg(
        long = "system-cpu-core-score",
        env = "GRADIENT_WORKER_SYSTEM_CPU_CORE_SCORE"
    )]
    pub cpu_core_score: Option<u32>,

    /// Free-RAM safety margin in MiB for the eval-subprocess reaper.
    /// Host `MemAvailable` below this margin is making the worker SIGKILL one eval subprocess.
    /// The victim is the one holding enough memory to restore the margin.
    /// Nothing is killed when no eval is that large.
    /// A value of `0` is selecting 10% of total RAM, clamped to [128 MiB, 1 GiB].
    #[arg(
        long = "system-min-free-ram-mb",
        env = "GRADIENT_WORKER_SYSTEM_MIN_FREE_RAM_MB",
        default_value_t = 0
    )]
    pub min_free_ram_mb: u64,
}

#[derive(Args, Debug, Clone)]
pub struct NixDaemonArgs {
    /// Maximum number of simultaneous connections in the local nix-daemon pool.
    /// Each in-flight prefetch NAR import is holding one connection for the whole upload.
    /// The pool must fit `build.max_concurrent * PREFETCH_CONCURRENCY` plus headroom.
    #[arg(
        long = "nix-daemon-max-connections",
        env = "GRADIENT_WORKER_NIX_DAEMON_MAX_CONNECTIONS",
        default_value_t = 32
    )]
    pub max_connections: usize,
}

impl Default for NixDaemonArgs {
    fn default() -> Self {
        Self {
            max_connections: 32,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct EvalArgs {
    /// Maximum number of simultaneous evaluations.
    #[arg(
        id = "eval-max-concurrent",
        long = "eval-max-concurrent",
        env = "GRADIENT_WORKER_EVAL_MAX_CONCURRENT",
        default_value_t = 1
    )]
    pub max_concurrent: u32,

    /// Number of parallel eval subprocesses in the pool.
    #[arg(long = "eval-fork-workers", env = "GRADIENT_WORKER_EVAL_FORK_WORKERS", default_value_t = default_fork_workers())]
    pub fork_workers: usize,

    /// Resident memory cap in bytes for one eval subprocess.
    /// A subprocess above this cap is recycled, and the next acquire is spawning a fresh one.
    /// The cap must stay above a typical eval's Boehm-GC heap to keep warm workers.
    #[arg(long = "eval-max-rss", env = "GRADIENT_WORKER_EVAL_MAX_RSS", default_value_t = 8 * 1024 * 1024 * 1024)]
    pub max_rss: u64,

    /// Directory for the Nix eval cache, exported to eval workers as `NIX_CACHE_HOME`.
    /// The default is `{base_dir}/eval-cache`.
    #[arg(long = "eval-cache-dir", env = "GRADIENT_WORKER_EVAL_CACHE_DIR")]
    pub cache_dir: Option<String>,

    /// Master switch for sharing `<fingerprint>.sqlite` eval-cache blobs across workers.
    #[arg(
        long = "eval-cache-share",
        env = "GRADIENT_WORKER_EVAL_CACHE_SHARE",
        default_value_t = true
    )]
    pub cache_share: bool,
}

impl Default for EvalArgs {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            fork_workers: default_fork_workers(),
            max_rss: 8 * 1024 * 1024 * 1024,
            cache_dir: None,
            cache_share: true,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct BuildArgs {
    /// Maximum number of simultaneous builds.
    #[arg(
        id = "build-max-concurrent",
        long = "build-max-concurrent",
        env = "GRADIENT_WORKER_BUILD_MAX_CONCURRENT",
        default_value_t = 1
    )]
    pub max_concurrent: u32,

    /// Cap on CPU cores a single build may use (nix `--cores` / `NIX_BUILD_CORES`).
    /// The default is all available cores.
    #[arg(long = "build-max-cores", env = "GRADIENT_WORKER_BUILD_MAX_CORES")]
    pub max_cores: Option<u32>,
}

impl Default for BuildArgs {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            max_cores: None,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct NarArgs {
    /// Upload requests this worker is keeping open at once, queued or transferring.
    /// Each presigned PUT is holding its compressed NAR in memory.
    #[arg(long = "nar-max-concurrent-uploads", env = "GRADIENT_WORKER_NAR_MAX_CONCURRENT_UPLOADS", default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_concurrent_uploads: u32,

    /// TTL in seconds for partial NAR downloads under `<base_dir>/nar-partial`.
    /// A periodic sweep is deleting partials with an older last write.
    /// A value of 0 is disabling the sweep.
    #[arg(
        long = "nar-partial-ttl-secs",
        env = "GRADIENT_WORKER_NAR_PARTIAL_TTL_SECS",
        default_value_t = 86400
    )]
    pub partial_ttl_secs: u64,
}

impl Default for NarArgs {
    fn default() -> Self {
        Self {
            max_concurrent_uploads: 16,
            partial_ttl_secs: 86400,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    #[arg(
        long = "log-level-default",
        env = "GRADIENT_WORKER_LOG_LEVEL_DEFAULT",
        default_value = "info"
    )]
    pub level_default: String,

    #[arg(long = "log-level-eval", env = "GRADIENT_WORKER_LOG_LEVEL_EVAL")]
    pub level_eval: Option<String>,

    #[arg(long = "log-level-build", env = "GRADIENT_WORKER_LOG_LEVEL_BUILD")]
    pub level_build: Option<String>,

    #[arg(long = "log-level-proto", env = "GRADIENT_WORKER_LOG_LEVEL_PROTO")]
    pub level_proto: Option<String>,

    /// Maximum log bytes forwarded per build in any 1-minute window.
    /// The worker is dropping further log output of a tripped build. The build is still running.
    #[arg(
        long = "log-burst-bytes-per-min",
        env = "GRADIENT_WORKER_LOG_BURST_BYTES_PER_MIN",
        default_value_t = 8 * 1024 * 1024
    )]
    pub burst_bytes_per_min: u64,

    /// Maximum log bytes forwarded per build in any 1-hour window.
    #[arg(
        long = "log-sustained-bytes-per-hour",
        env = "GRADIENT_WORKER_LOG_SUSTAINED_BYTES_PER_HOUR",
        default_value_t = 64 * 1024 * 1024
    )]
    pub sustained_bytes_per_hour: u64,

    /// Forward the existing nix-store build log of a derivation with locally built outputs.
    #[arg(
        long = "log-fetch-from-store",
        env = "GRADIENT_WORKER_LOG_FETCH_FROM_STORE",
        default_value = "true"
    )]
    pub fetch_from_store: bool,

    /// Directory receiving every closed stage span as JSON lines, one file per process.
    /// Span tracing is off without it.
    #[arg(long = "log-trace-dir", env = "GRADIENT_WORKER_LOG_TRACE_DIR")]
    pub trace_dir: Option<std::path::PathBuf>,
}

impl Default for LogArgs {
    fn default() -> Self {
        Self {
            level_default: "info".to_owned(),
            level_eval: None,
            level_build: None,
            level_proto: None,
            burst_bytes_per_min: 8 * 1024 * 1024,
            sustained_bytes_per_hour: 64 * 1024 * 1024,
            fetch_from_store: true,
            trace_dir: None,
        }
    }
}

pub fn host_system() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{arch}-{os}")
}

impl WorkerConfig {
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

    pub fn accepted_server_tokens(&self) -> Option<Vec<(String, String)>> {
        let path = self.accepted_server_tokens_file.as_ref()?;
        match std::fs::read_to_string(path) {
            Ok(raw) => Some(parse_token_hashes(&raw)),
            Err(e) => {
                tracing::warn!(path, error = %e, "failed to read accepted server tokens file; rejecting every server");
                Some(Vec::new())
            }
        }
    }

    pub fn nar_partial_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.base_dir).join("nar-partial")
    }

    pub fn eval_cache_dir(&self) -> String {
        self.eval
            .cache_dir
            .clone()
            .unwrap_or_else(|| format!("{}/eval-cache", self.base_dir))
    }

    pub fn build_cores(&self) -> u32 {
        self.build.max_cores.unwrap_or(0)
    }

    pub fn cpu_core_score(&self) -> u32 {
        self.system
            .cpu_core_score
            .unwrap_or_else(crate::metrics::cpu_core_score)
    }

    pub fn capabilities(&self) -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: self.capabilities.federate,
            fetch: self.capabilities.fetch,
            eval: self.capabilities.eval,
            build: self.capabilities.build,
            cache: false,
        }
    }
}

fn parse_token_hashes(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(':'))
        .map(|(peer, hash)| (peer.trim().to_owned(), hash.trim().to_owned()))
        .filter(|(peer, hash)| !peer.is_empty() && !hash.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn config_with_peers(peers: &str) -> WorkerConfig {
        WorkerConfig {
            peers: Some(peers.to_owned()),
            ..Default::default()
        }
    }

    const TOK64: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    const TOK63: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    const TOK65: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

    #[test]
    fn arguments_are_unique() {
        WorkerConfig::command().debug_assert();
    }

    #[test]
    fn every_env_is_the_prefixed_flag() {
        let mismatched: Vec<String> = WorkerConfig::command()
            .get_arguments()
            .filter_map(|arg| {
                let long = arg.get_long()?;
                if matches!(long, "help" | "version") {
                    return None;
                }

                let expected = format!("GRADIENT_WORKER_{}", long.to_uppercase().replace('-', "_"));
                let env = arg.get_env().map(|e| e.to_string_lossy().into_owned());
                (env.as_deref() != Some(expected.as_str())).then(|| format!("--{long}: {env:?}"))
            })
            .collect();

        assert!(mismatched.is_empty(), "{mismatched:#?}");
    }

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
        let cfg = WorkerConfig::default();
        assert!(cfg.peer_tokens().is_empty());
    }

    #[test]
    fn peer_tokens_skips_empty_peer_or_token() {
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
            peers: Some(format!("peer-inline:{TOK64}")),
            peers_file: Some(path.clone()),
            ..Default::default()
        };
        let tokens = cfg.peer_tokens();
        let _ = std::fs::remove_file(&path);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-file");
    }

    #[test]
    fn accepted_server_tokens_are_absent_without_a_file() {
        assert!(WorkerConfig::default().accepted_server_tokens().is_none());
    }

    #[test]
    fn accepted_server_tokens_read_peer_hash_lines() {
        let path =
            std::env::temp_dir().join(format!("gradient-test-accepted-{}", std::process::id()));
        std::fs::write(
            &path,
            "# servers\np1:$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA\n\n*:abc123\nbroken\n",
        )
        .unwrap();
        let cfg = WorkerConfig {
            accepted_server_tokens_file: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };

        let tokens = cfg.accepted_server_tokens();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            tokens,
            Some(vec![
                (
                    "p1".to_string(),
                    "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA".to_string()
                ),
                ("*".to_string(), "abc123".to_string()),
            ])
        );
    }

    #[test]
    fn an_unreadable_accepted_server_tokens_file_rejects_every_server() {
        let cfg = WorkerConfig {
            accepted_server_tokens_file: Some(
                "/nonexistent/gradient-accepted-server-tokens".into(),
            ),
            ..Default::default()
        };
        assert_eq!(cfg.accepted_server_tokens(), Some(vec![]));
    }
}
