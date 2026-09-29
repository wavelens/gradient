/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::{Args, Parser};
use gradient_wire::types::GradientCapabilities;

/// Default eval-pool size: host parallelism capped at 16. Each worker may hold
/// up to `eval.max_rss` resident, so the cap bounds eval memory on big hosts.
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
    /// Example: `wss://gradient.example.com/proto`
    #[arg(long = "server-url", env = "GRADIENT_WORKER_SERVER_URL")]
    pub server_url: String,

    /// Peer-to-token mappings for challenge-response authentication.
    /// Format: one `peer_id:token` pair per line (newline-separated).
    /// Use `*:token` to respond with `token` for any peer UUID the server challenges.
    /// Each named peer is a project, cache, or proxy UUID.
    /// Mutually exclusive with `--peers-file`.
    #[arg(long = "peers", env = "GRADIENT_WORKER_PEERS")]
    pub peers: Option<String>,

    /// Path to a file whose contents are peer-to-token pairs, one per line
    /// (same format as `--peers`). Takes precedence over `--peers`.
    #[arg(long = "peers-file", env = "GRADIENT_WORKER_PEERS_FILE")]
    pub peers_file: Option<String>,

    /// Directory for persistent worker state (worker ID file, etc.).
    /// Defaults to `/var/lib/gradient-worker`. Must be writable.
    #[arg(
        long = "base-dir",
        env = "GRADIENT_WORKER_BASE_DIR",
        default_value = "/var/lib/gradient-worker"
    )]
    pub base_dir: String,

    /// Override the worker's persistent UUID. When set, this value is used as
    /// the worker identity instead of the UUID stored in `{base_dir}/worker-id`.
    /// Useful for declarative deployments where the ID must be known before the
    /// worker first runs. Must be a valid UUID.
    #[arg(long = "id", env = "GRADIENT_WORKER_ID")]
    pub id: Option<String>,

    /// Path to the `nix` binary. Defaults to `nix` (resolved via `PATH`).
    #[arg(
        long = "nix-bin",
        env = "GRADIENT_WORKER_NIX_BIN",
        default_value = "nix"
    )]
    pub nix_bin: String,

    /// Path to the `ssh` binary. Used as `GIT_SSH_COMMAND` when nix fetches
    /// private flake inputs. Defaults to `ssh` (resolved via `PATH`).
    #[arg(
        long = "ssh-bin",
        env = "GRADIENT_WORKER_SSH_BIN",
        default_value = "ssh"
    )]
    pub ssh_bin: String,

    /// Directory under which worker-held indirect GC roots are written.
    /// One symlink per active build (drv + outputs) pins inputs and
    /// outputs through the daemon while the build runs. Empty string
    /// disables GC root pinning (build still works, but a concurrent
    /// `nix-collect-garbage` may race the build).
    #[arg(
        long = "gcroots-dir",
        env = "GRADIENT_WORKER_GCROOTS_DIR",
        default_value = "/nix/var/nix/gcroots/gradient"
    )]
    pub gcroots_dir: String,

    /// How long a SIGINT/SIGTERM drain waits for in-flight jobs before giving
    /// up on them. The worker stops accepting work immediately, finishes what
    /// is running, reports it, and exits; jobs still running at the deadline
    /// are aborted and re-queued server-side. Default 600 (10 min). Set to 0
    /// to wait indefinitely, and keep the unit's `TimeoutStopSec` above this.
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

    /// IP address on which to listen for incoming server connections when discoverable.
    #[arg(
        long = "listen-addr",
        env = "GRADIENT_WORKER_LISTEN_ADDR",
        default_value = "127.0.0.1"
    )]
    pub listen_addr: String,

    /// Port on which to listen for incoming server connections when discoverable.
    #[arg(long = "port", env = "GRADIENT_WORKER_PORT", default_value_t = 3100)]
    pub port: u16,

    /// Re-exec as a Nix evaluator subprocess (internal - do not set manually).
    #[arg(
        long = "eval-subprocess",
        env = "GRADIENT_WORKER_EVAL_SUBPROCESS",
        hide = true
    )]
    pub eval_subprocess: bool,

    /// Drive one eval subprocess over the production rkyv transport
    /// from a JSONL request file, printing JSON responses (internal test
    /// harness for the NixOS VM integration test).
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
            nix_bin: "nix".to_owned(),
            ssh_bin: "ssh".to_owned(),
            gcroots_dir: "/nix/var/nix/gcroots/gradient".to_owned(),
            drain_timeout_secs: 600,
            discoverable: false,
            listen_addr: "127.0.0.1".to_owned(),
            port: 3100,
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
    /// Relay work and NAR traffic between workers and servers (federation).
    /// Requires `--discoverable`.
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
    /// Defaults to the host system (e.g. `x86_64-linux`). Override to add
    /// emulated targets - e.g. `x86_64-linux,aarch64-linux` on a binfmt host.
    /// Used by the server's dispatcher to gate build assignment.
    #[arg(
        long = "system-architectures",
        env = "GRADIENT_WORKER_SYSTEM_ARCHITECTURES",
        value_delimiter = ','
    )]
    pub architectures: Option<Vec<String>>,

    /// Comma-separated Nix system features this worker advertises
    /// (e.g. `kvm,big-parallel,nixos-test`). Builds requiring features not
    /// in this list won't be assigned to this worker. When unset, the worker
    /// auto-detects them from `nix config show system-features` (the daemon's
    /// resolved set, including CPU-derived `gccarch-*` levels); set this to
    /// override that.
    #[arg(
        long = "system-features",
        env = "GRADIENT_WORKER_SYSTEM_FEATURES",
        value_delimiter = ','
    )]
    pub features: Option<Vec<String>>,

    /// Override the single-core speed score advertised to the scheduler.
    /// When unset, the worker runs a deterministic micro-benchmark at startup.
    #[arg(
        long = "system-cpu-core-score",
        env = "GRADIENT_WORKER_SYSTEM_CPU_CORE_SCORE"
    )]
    pub cpu_core_score: Option<u32>,

    /// Free-RAM safety margin in MiB for the eval-subprocess reaper. When host
    /// `MemAvailable` falls below this, the worker SIGKILLs the one live eval
    /// subprocess holding enough resident memory to bring it back above the
    /// margin (the parent then reports the eval as failed instead of the machine
    /// freezing); when no eval is that large the pressure is not coming from
    /// evaluation and nothing is killed. `0` selects an adaptive margin of
    /// `10% of total RAM, clamped to [128 MiB, 1 GiB]`. `eval.max_rss` still
    /// bounds steady-state RSS; this is the proactive peak guard.
    #[arg(
        long = "system-min-free-ram-mb",
        env = "GRADIENT_WORKER_SYSTEM_MIN_FREE_RAM_MB",
        default_value_t = 0
    )]
    pub min_free_ram_mb: u64,
}

#[derive(Args, Debug, Clone)]
pub struct NixDaemonArgs {
    /// Maximum number of simultaneous connections in the local nix-daemon
    /// pool. Each in-flight `add_to_store_nar` (NAR import during prefetch)
    /// holds one connection for the duration of the upload, so the pool
    /// must comfortably fit `build.max_concurrent *
    /// prefetch::PREFETCH_CONCURRENCY` plus headroom for `has_path`
    /// probes and build dispatch.
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

    /// Number of parallel eval subprocesses in the pool (the eval concurrency).
    #[arg(long = "eval-fork-workers", env = "GRADIENT_WORKER_EVAL_FORK_WORKERS", default_value_t = default_fork_workers())]
    pub fork_workers: usize,

    /// Safety cap on an eval subprocess's resident memory: once its RSS exceeds
    /// this many bytes it is recycled (parent-side) so the next acquire spawns a
    /// fresh one. Keep it above a typical eval's Boehm-GC heap (default 8 GiB) so
    /// it bounds runaway growth without recycling warm workers mid-evaluation.
    #[arg(long = "eval-max-rss", env = "GRADIENT_WORKER_EVAL_MAX_RSS", default_value_t = 8 * 1024 * 1024 * 1024)]
    pub max_rss: u64,

    /// Directory holding the Nix eval cache (exported to eval workers as
    /// `NIX_CACHE_HOME`). When unset, resolves to `{base_dir}/eval-cache`.
    #[arg(long = "eval-cache-dir", env = "GRADIENT_WORKER_EVAL_CACHE_DIR")]
    pub cache_dir: Option<String>,

    /// Master switch for fleet eval-cache sharing (pull/push of
    /// `<fingerprint>.sqlite` blobs across workers).
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
    /// Unset (the default) passes `0` to the daemon, meaning all available cores.
    #[arg(long = "build-max-cores", env = "GRADIENT_WORKER_BUILD_MAX_CORES")]
    pub max_cores: Option<u32>,

    /// Capture per-build resource metrics (peak RAM, CPU time, disk I/O) from
    /// the build's cgroup. Requires Nix's experimental `use-cgroups` feature on
    /// the daemon. Wall-clock build time is always reported regardless.
    #[arg(
        long = "build-metrics",
        env = "GRADIENT_WORKER_BUILD_METRICS",
        default_value = "false"
    )]
    pub metrics: bool,

    /// The nix daemon's cgroup, in which it creates each build's
    /// `nix-build@<drv-hash>-<uid>` cgroup when `--build-metrics` is enabled.
    #[arg(
        long = "build-cgroup-root",
        env = "GRADIENT_WORKER_BUILD_CGROUP_ROOT",
        default_value = "/sys/fs/cgroup/system.slice/nix-daemon.service"
    )]
    pub cgroup_root: String,
}

impl Default for BuildArgs {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            max_cores: None,
            metrics: false,
            cgroup_root: "/sys/fs/cgroup/system.slice/nix-daemon.service".to_owned(),
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct NarArgs {
    /// Upload requests this worker keeps open at once (queued at the server or
    /// transferring). Bounds memory: a presigned PUT holds its compressed NAR.
    #[arg(long = "nar-max-concurrent-uploads", env = "GRADIENT_WORKER_NAR_MAX_CONCURRENT_UPLOADS", default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_concurrent_uploads: u32,

    /// TTL in seconds for partially-received NAR downloads (`*.partial`) staged
    /// under `<base_dir>/nar-partial`. A periodic sweep deletes partials whose
    /// last write is older than this so an abandoned resume can't pin disk
    /// forever. Default 86400 (24 h). Set to 0 to disable the sweep.
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

    /// Burst bucket: max log bytes forwarded to the server per build in any
    /// 1-minute window. Defaults to 8 MiB. On trip the worker stops forwarding
    /// log output for that build (the build still runs).
    #[arg(
        long = "log-burst-bytes-per-min",
        env = "GRADIENT_WORKER_LOG_BURST_BYTES_PER_MIN",
        default_value_t = 8 * 1024 * 1024
    )]
    pub burst_bytes_per_min: u64,

    /// Sustained bucket: max log bytes forwarded to the server per build in any
    /// 1-hour window. Defaults to 64 MiB.
    #[arg(
        long = "log-sustained-bytes-per-hour",
        env = "GRADIENT_WORKER_LOG_SUSTAINED_BYTES_PER_HOUR",
        default_value_t = 64 * 1024 * 1024
    )]
    pub sustained_bytes_per_hour: u64,

    /// Fetch a derivation's existing nix-store build log (`.bz2`) and forward it
    /// when the outputs are already built locally and no new log is produced.
    #[arg(
        long = "log-fetch-from-store",
        env = "GRADIENT_WORKER_LOG_FETCH_FROM_STORE",
        default_value = "true"
    )]
    pub fetch_from_store: bool,

    /// Directory that receives every closed stage span as JSON lines, one file
    /// per process; eval subprocesses write their own. Unset disables span tracing.
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

/// Detect the host's Nix system string from `std::env::consts`.
/// Maps `std::env::consts::OS` (`macos`) to Nix's convention (`darwin`).
pub fn host_system() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{arch}-{os}")
}

impl WorkerConfig {
    /// Parse peer-to-token pairs from `--peers-file` (preferred) or `--peers`.
    /// Returns an empty vec when neither is set (open/discoverable mode).
    ///
    /// Format: one `peer_id:token` entry per line. The special peer ID `*`
    /// matches any UUID the server challenges - callers should expand it using
    /// `resolve_tokens_for_challenge`.
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

    /// Directory under which partially-received NAR downloads are staged for
    /// resume. A subdirectory of `base_dir` so the existing `StateDirectory`
    /// covers it.
    pub fn nar_partial_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.base_dir).join("nar-partial")
    }

    /// Resolved eval-cache directory: the configured override or, by default,
    /// `{base_dir}/eval-cache`.
    pub fn eval_cache_dir(&self) -> String {
        self.eval
            .cache_dir
            .clone()
            .unwrap_or_else(|| format!("{}/eval-cache", self.base_dir))
    }

    /// Resolve the nix `--cores` value for a build: the configured cap, or `0`
    /// (all available cores) when unset.
    pub fn build_cores(&self) -> u32 {
        self.build.max_cores.unwrap_or(0)
    }

    /// Build the `GradientCapabilities` struct from the CLI flags.
    pub fn capabilities(&self) -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: self.capabilities.federate,
            fetch: self.capabilities.fetch,
            eval: self.capabilities.eval,
            build: self.capabilities.build,
            cache: false, // workers never serve as cache
        }
    }
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

    /// 64 `x` characters - the only accepted token length.
    const TOK64: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    /// 63 `x` characters - one too short.
    const TOK63: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    /// 65 `x` characters - one too long.
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
        let cfg = WorkerConfig::default();
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
            peers: Some(format!("peer-inline:{TOK64}")),
            peers_file: Some(path.clone()),
            ..Default::default()
        };
        let tokens = cfg.peer_tokens();
        let _ = std::fs::remove_file(&path);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-file");
    }
}
