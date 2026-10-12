/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use clap::{Args, Parser};
use gradient_derive::config_group;

/// CLI arguments and environment variables for `gradient-worker`.
#[config_group(env = "worker", no_default)]
#[derive(Parser, Debug, Clone)]
#[command(name = "gradient-worker", about = "Gradient build worker")]
pub struct WorkerConfig {
    /// WebSocket URL of the Gradient server's `/proto` endpoint. `null` leaves the worker
    /// waiting for the server to connect.
    pub server_url: String,

    /// Peer-to-token mappings for challenge-response authentication.
    /// Each line is holding one `peer_id:token` pair.
    /// A `*:token` line is answering every challenged peer UUID with `token`.
    /// Each named peer is a project, cache, or proxy UUID.
    /// This option is mutually exclusive with `--peers-file`.
    pub peers: Option<String>,

    /// File of peer tokens for challenge-response authentication with the server, one
    /// `peer_id:token` per line. Lines starting with `#` are ignored.
    ///
    /// ```
    /// <uuid>:<token>
    /// *:<token>
    /// ```
    ///
    /// The peer ID `*` matches any UUID in the server's challenge. Each token is a 48 byte
    /// random secret, for example from {command}`openssl rand -base64 48`, registered through
    /// `POST /api/v1/projects/{project}/workers`. Pin a project UUID with
    /// {option}`services.gradient.state.projects.<name>.id` to reference it here.
    ///
    /// `null` connects in open mode, where the server accepts the worker without a token.
    pub peers_file: Option<PathBuf>,

    /// Directory holding the worker's state.
    #[arg(default_value = "/var/lib/gradient-worker")]
    pub base_dir: PathBuf,

    /// Worker UUID. `null` generates one on first start and stores it in
    /// {file}`<services.gradient.worker.baseDir>/worker-id`. Set it when the UUID must be known in
    /// advance, for example to register the worker in {option}`services.gradient.state.workers`.
    pub id: Option<String>,

    /// Locality label advertised to the scheduler. A cluster job asking for one zone places
    /// every member on workers with the same label. `null` puts the worker in the zone of
    /// all unlabelled workers.
    pub zone: Option<String>,

    /// Address at which other members of a cluster job reach this worker, passed through
    /// verbatim in the cluster roster. `null` advertises none.
    pub endpoint: Option<String>,

    /// Path to the `nix` binary, resolved via `PATH` by default.
    #[arg(default_value = "nix")]
    pub nix_bin: String,

    /// Path to the `ssh` binary, resolved via `PATH` by default.
    /// Nix is using it as `GIT_SSH_COMMAND` for private flake inputs.
    #[arg(default_value = "ssh")]
    pub ssh_bin: String,

    /// Directory for the indirect GC roots pinning each running build's inputs and outputs against
    /// a concurrent {command}`nix-collect-garbage`. An empty string disables pinning.
    #[arg(default_value = "/nix/var/nix/gcroots/gradient")]
    pub gcroots_dir: String,

    /// Whether to enable incoming server connections on `/proto`.
    #[arg(default_value = "false")]
    pub discoverable: bool,

    /// IP address the worker listens on for incoming server connections.
    #[arg(default_value = "127.0.0.1")]
    pub listen_addr: String,

    /// Port the worker listens on for incoming server connections.
    #[arg(default_value_t = 3100)]
    pub port: u16,

    /// File of token hashes a server must present when it connects to this worker, one
    /// `peer_id:hash` per line. Lines starting with `#` are ignored. A hash is an argon2 PHC
    /// string or the lowercase SHA-256 hex digest of the token, for example from
    /// {command}`printf %s "$TOKEN" | sha256sum`. The peer ID `*` matches any peer. The file
    /// is only read with {option}`services.gradient.worker.discoverable`.
    ///
    /// `null` accepts every server, and the worker logs a warning at start.
    pub accepted_server_tokens_file: Option<PathBuf>,

    /// Re-exec as a Nix evaluator subprocess (internal - do not set manually).
    #[arg(hide = true)]
    pub eval_subprocess: bool,

    /// Drive one eval subprocess over the production rkyv transport from a JSONL request file.
    /// This internal harness is serving the NixOS VM integration test.
    #[arg(hide = true)]
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
            base_dir: PathBuf::from("/var/lib/gradient-worker"),
            id: None,
            zone: None,
            endpoint: None,
            nix_bin: "nix".to_owned(),
            ssh_bin: "ssh".to_owned(),
            gcroots_dir: "/nix/var/nix/gcroots/gradient".to_owned(),
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

#[config_group("capabilities", env = "worker_capabilities")]
#[derive(Args, Debug, Clone)]
pub struct CapabilitiesArgs {
    /// Whether to enable forwarding work and NARs between workers and servers (requiring `discoverable`).
    #[arg(default_value = "false")]
    pub federate: bool,

    /// Whether to enable prefetching flake inputs and sources.
    #[arg(default_value = "true")]
    pub fetch: bool,

    /// Whether to enable Nix flake evaluations.
    #[arg(default_value = "true")]
    pub eval: bool,

    /// Whether to enable Nix builds.
    #[arg(default_value = "true")]
    pub build: bool,
}

#[config_group("system", env = "worker_system")]
#[derive(Args, Debug, Clone)]
pub struct SystemArgs {
    /// Nix system types this worker builds for.
    #[arg(value_delimiter = ',')]
    pub architectures: Vec<String>,

    /// Nix system features this worker advertises. An empty list detects them at runtime from
    /// {command}`nix config show system-features`, including CPU-derived `gccarch-*` levels.
    #[arg(value_delimiter = ',')]
    pub features: Vec<String>,

    /// Single-core speed score advertised to the scheduler, higher is faster. `null`
    /// benchmarks the host at startup.
    pub cpu_core_score: Option<NonZeroU32>,

    /// Free memory in MiB below which the worker kills the one evaluation subprocess large
    /// enough to restore the margin. The worker reports that evaluation as failed instead of
    /// letting the host freeze. `0` uses 10% of total memory, clamped to 128 MiB to 1 GiB.
    /// {option}`services.gradient.worker.eval.maxRss` still bounds steady-state memory.
    #[arg(default_value_t = 0)]
    pub min_free_ram_mb: u64,
}

#[config_group("nix_daemon", env = "worker_nix_daemon")]
#[derive(Args, Debug, Clone)]
pub struct NixDaemonArgs {
    /// Maximum connections to the local Nix daemon. Each build holds one for its whole run
    /// plus up to 8 for parallel NAR imports. The rest is headroom for path checks.
    #[arg(default_value = "32")]
    pub max_connections: NonZeroUsize,
}

#[config_group("eval", env = "worker_eval")]
#[derive(Args, Debug, Clone)]
pub struct EvalArgs {
    /// Maximum simultaneous evaluations.
    #[arg(id = "eval-max-concurrent", default_value = "1")]
    pub max_concurrent: NonZeroU32,

    /// Evaluation subprocesses in the pool, equal to the evaluation concurrency. `null` sizes
    /// the pool to the host's core count, capped at 16.
    pub fork_workers: Option<NonZeroUsize>,

    /// Resident memory in bytes for an idle evaluation subprocess. Subprocesses above the limit
    /// restart after the call and free the Nix heap. A call growing past the limit is fine, with
    /// {option}`services.gradient.worker.system.minFreeRamMb` as the host's guard.
    /// Keep it above a typical evaluation's heap.
    #[arg(default_value = "2147483648")]
    pub max_rss: NonZeroU64,

    /// Whether to collect per-evaluation Nix statistics (thunks, heap, peak memory, hotspots,
    /// flake graph). Disable it to leave that overhead out.
    #[arg(default_value = "true")]
    pub metrics: bool,

    /// Eval cache directory, exported to evaluation subprocesses as {env}`NIX_CACHE_HOME`.
    /// `null` uses {file}`<services.gradient.worker.baseDir>/eval-cache`.
    pub cache_dir: Option<String>,

    /// Whether to share eval cache blobs with other workers through the server.
    #[arg(default_value_t = true)]
    pub cache_share: bool,

    /// Ceiling in MiB for the resident memory of all evaluation subprocesses together.
    /// Subprocesses leave the pool, largest first, while the sum is over the ceiling.
    /// Conflicts with {option}`services.gradient.worker.system.minFreeRamMb`.
    pub max_memory: Option<NonZeroU64>,
}

#[config_group("build", env = "worker_build")]
#[derive(Args, Debug, Clone)]
pub struct BuildArgs {
    /// Maximum simultaneous builds.
    #[arg(id = "build-max-concurrent", default_value = "32")]
    pub max_concurrent: NonZeroU32,

    /// CPU cores for a build, passed as `--cores`. `null` for every core.
    pub max_cores: Option<NonZeroU32>,

    /// Cgroup directory of {file}`nix-daemon.service`, the parent of the build cgroups, read for
    /// the peak memory of every running build and sent with the heartbeat. The larger of the
    /// predicted and the reported peak.
    /// The build cgroups need {option}`services.gradient.worker.build.metrics` and Gradient's
    /// Nix fork on the daemon. `null` reports nothing.
    pub cgroup: Option<PathBuf>,
}

#[config_group("nar", env = "worker_nar")]
#[derive(Args, Debug, Clone)]
pub struct NarArgs {
    /// Upload requests over 1 MiB open at once, waiting for a server grant or transferring.
    /// Half of the limit for any job. A window of 128 for smaller uploads.
    /// How many run: the server's upload budget. A bound on worker memory.
    #[arg(default_value = "16")]
    pub max_concurrent_uploads: NonZeroU32,

    /// Seconds after the last write before the deletion of an unfinished NAR download under
    /// {file}`<services.gradient.worker.baseDir>/nar-partial`. `0` for no cleanup.
    #[arg(default_value_t = 86400)]
    pub partial_ttl_secs: u64,

    /// Size in bytes of each outbound NAR upload chunk, from 4 KiB to 4 MiB.
    /// Smaller chunks can help slow or high-latency links like Tailscale over DERP.
    #[arg(
        value_parser = clap::value_parser!(u64).range(gradient_types::NAR_CHUNK_BYTES_RANGE),
        default_value_t = 512 * 1024,
    )]
    pub chunk_bytes: u64,
}

#[config_group("log", env = "worker_log")]
#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    #[arg(default_value = "info")]
    pub level_default: String,

    pub level_eval: Option<String>,

    pub level_build: Option<String>,

    pub level_proto: Option<String>,

    /// Build log bytes forwarded per build within any minute. The worker stops forwarding
    /// a build's log past this limit. The build itself continues.
    #[arg(default_value_t = 8 * 1024 * 1024)]
    pub burst_bytes_per_min: u64,

    /// Build log bytes forwarded per build within any hour.
    #[arg(default_value_t = 64 * 1024 * 1024)]
    pub sustained_bytes_per_hour: u64,

    /// Whether to forward the stored build log of a derivation already built locally and
    /// therefore producing no new log.
    #[arg(default_value = "true")]
    pub fetch_from_store: bool,

    /// Directory receiving every closed stage span of the worker and its eval subprocesses as
    /// JSON lines, one file per process. `null` disables span tracing.
    pub trace_dir: Option<PathBuf>,
}
