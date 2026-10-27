/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser};
use gradient_derive::config_group;
use gradient_vm::{DeviceConfig, MountConfig, check_device};
use serde::de::DeserializeOwned;

/// CLI arguments and environment variables for `gradient-worker`.
#[config_group(env = "worker", no_default)]
#[derive(Parser, Debug, Clone)]
#[command(name = "gradient-worker", about = "Gradient build worker")]
pub struct WorkerConfig {
    /// WebSocket URL of the Gradient server's `/proto` endpoint. `null` skips the outbound
    /// connection, leaving only incoming server connections through
    /// {option}`services.gradient.worker.discoverable`.
    pub server_url: Option<String>,

    /// Peer-to-token mappings for challenge-response authentication.
    /// Each line holds one `peer_id:token` pair.
    /// A `*:token` line answers every challenged peer UUID with `token`.
    /// Each named peer is a project, cache, or proxy UUID.
    /// Mutually exclusive with `--peers-file`.
    pub peers: Option<String>,

    /// File of peer tokens for challenge-response authentication with the server,
    /// with `peer_id:token` per line and `#` starting a comment.
    ///
    /// ```
    /// <uuid>:<token>
    /// *:<token>
    /// ```
    ///
    /// Use peer ID `*` to match any UUID in the server's challenge.
    /// Tokens are 48 byte random secrets, for example from {command}`openssl rand -base64 48`.
    /// Register them via the API through `POST /api/v1/projects/{project}/workers`.
    /// Set it to `null` for open mode without authentication; avoid that in a production setup.
    pub peers_file: Option<PathBuf>,

    /// Directory holding the worker's state.
    #[arg(default_value = "/var/lib/gradient-worker")]
    pub base_dir: PathBuf,

    /// Worker UUID. `null` generates one on first start and stores it in
    /// {file}`<services.gradient.worker.baseDir>/worker-id`. Set it when the UUID must be known
    /// in advance, for example to register the worker in
    /// {option}`services.gradient.state.workers`.
    pub id: Option<String>,

    /// Locality label advertised to the scheduler. A cluster job asking for one zone places
    /// every member on workers with the same label. `null` places the worker among the
    /// unlabelled workers.
    pub zone: Option<String>,

    /// Address at which other members of a cluster job connect to this worker, passed through
    /// verbatim in the cluster roster. `null` advertises none.
    pub endpoint: Option<String>,

    /// Path to the `ssh` binary, resolved via `PATH` by default.
    /// Nix uses it as `GIT_SSH_COMMAND` for private flake inputs.
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

    /// File of token hashes required from connecting servers,
    /// with `peer_id:hash` per line and `#` starting a comment.
    /// A hash is an argon2 PHC string or the lowercase SHA-256 hex digest of the token itself,
    /// for example from {command}`printf %s "$TOKEN" | sha256sum`.
    /// Peer ID `*` matches any peer.
    /// The worker reads this file only with {env}`GRADIENT_WORKER_DISCOVERABLE`.
    /// Set it to `null` to accept every server; avoid that in a production setup.
    pub accepted_server_tokens_file: Option<PathBuf>,

    /// Seconds without an assigned job after which the worker drains and exits. `null` keeps it
    /// running.
    pub idle_timeout: Option<u64>,

    /// Seconds after start after which the worker drains, finishes its running jobs and exits.
    /// `null` keeps it running.
    pub max_lifetime: Option<u64>,

    /// Re-exec as a Nix evaluator subprocess (internal - do not set manually).
    #[arg(hide = true)]
    pub eval_subprocess: bool,

    /// Drive one eval subprocess over the production rkyv transport from a JSONL request file.
    /// This internal harness serves the NixOS VM integration test.
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
    #[command(flatten)]
    pub exec: ExecArgs,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            server_url: None,
            peers: None,
            peers_file: None,
            base_dir: PathBuf::from("/var/lib/gradient-worker"),
            id: None,
            zone: None,
            endpoint: None,
            ssh_bin: "ssh".to_owned(),
            gcroots_dir: "/nix/var/nix/gcroots/gradient".to_owned(),
            discoverable: false,
            listen_addr: "127.0.0.1".to_owned(),
            port: 3100,
            accepted_server_tokens_file: None,
            idle_timeout: None,
            max_lifetime: None,
            eval_subprocess: false,
            eval_driver: None,
            capabilities: CapabilitiesArgs::default(),
            system: SystemArgs::default(),
            nix_daemon: NixDaemonArgs::default(),
            eval: EvalArgs::default(),
            build: BuildArgs::default(),
            nar: NarArgs::default(),
            log: LogArgs::default(),
            exec: ExecArgs::default(),
        }
    }
}

#[config_group("capabilities", env = "worker_capabilities")]
#[derive(Args, Debug, Clone)]
pub struct CapabilitiesArgs {
    /// Whether to forward work and NARs between workers and servers (requires `discoverable`).
    #[arg(default_value = "false")]
    pub federate: bool,

    /// Whether to run Nix flake evaluations.
    #[arg(default_value = "true")]
    pub eval: bool,

    /// Whether to run Nix builds.
    #[arg(default_value = "true")]
    pub build: bool,

    /// Whether this worker may reach the internet. Cloning repositories, fetching flake inputs,
    /// substitutions, downloads and fixed-output builds run only on workers with it.
    #[arg(default_value = "true")]
    pub network: bool,

    /// Whether the server may send project credentials such as SSH keys to this worker.
    #[arg(default_value = "true")]
    pub secrets: bool,

    /// Whether this worker takes exec jobs. Exec jobs also need the server option `exec.enable`,
    /// the project switch `exec_enabled` and `enable_exec` on the worker registration.
    #[arg(default_value = "false")]
    pub exec: bool,
}

#[config_group("system", env = "worker_system")]
#[derive(Args, Debug, Clone)]
pub struct SystemArgs {
    /// Nix system types this worker builds for.
    #[arg(value_delimiter = ',')]
    pub architectures: Vec<String>,

    /// Nix system features this worker advertises. An empty list uses the system features of
    /// the Nix store, including those that enabled experimental features imply, such as
    /// `recursive-nix`.
    #[arg(value_delimiter = ',')]
    pub features: Vec<String>,

    /// Single-core speed score advertised to the scheduler; higher is faster. `null`
    /// benchmarks the host at startup.
    pub cpu_core_score: Option<NonZeroU32>,

    /// Free memory in MiB below which the worker kills the one evaluation subprocess large
    /// enough to restore the margin and reports that evaluation as failed instead of letting
    /// the host freeze. `0` uses 10% of total memory, clamped to 128 MiB to 1 GiB.
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
    /// restart after the call and free the Nix heap. A call growing past the limit is fine,
    /// with {option}`services.gradient.worker.system.minFreeRamMb` as the host's guard. Keep it
    /// above a typical evaluation's heap.
    #[arg(default_value = "2147483648")]
    pub max_rss: NonZeroU64,

    /// Whether to collect per-evaluation Nix statistics (thunks, heap, peak memory, hotspots,
    /// flake graph). Disable it to skip that overhead.
    #[arg(default_value = "true")]
    pub metrics: bool,

    /// Eval cache directory, exported to evaluation subprocesses as {env}`NIX_CACHE_HOME`.
    /// `null` uses {file}`<services.gradient.worker.baseDir>/eval-cache`.
    pub cache_dir: Option<String>,

    /// Whether to share the eval cache with other workers through the server. Evaluations pull
    /// the segments of other workers first and upload new rows as segments while they run.
    #[arg(default_value_t = true)]
    pub cache_share: bool,

    /// Ceiling in MiB for the resident memory of all evaluation subprocesses together.
    /// Subprocesses leave the pool largest first while the total exceeds the ceiling.
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
    /// Half of the limit for any job, a window of 128 for smaller uploads.
    /// The server's upload budget decides how many run and bounds the worker's memory.
    #[arg(default_value = "16")]
    pub max_concurrent_uploads: NonZeroU32,

    /// Seconds after the last write before the worker deletes an unfinished NAR download under
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

#[config_group("exec", env = "worker_exec")]
#[derive(Args, Debug, Clone)]
pub struct ExecArgs {
    /// Guest image from `gradient.lib.mkWorkerImage` that exec job guests boot.
    pub image: Option<PathBuf>,

    /// Guests booted from {option}`services.gradient.worker.exec.image` and paused, waiting
    /// for a job.
    #[arg(default_value_t = 0)]
    pub pre_warm: u32,

    /// Most guests kept warm for upcoming exec jobs the server reports. The worker keeps the
    /// larger of {option}`services.gradient.worker.exec.preWarm` and the reported count, capped
    /// at this value. `0` ignores the server count.
    #[arg(default_value_t = 0)]
    pub max_pre_warm: u32,

    /// Whether exec jobs may boot their own guest image instead of
    /// {option}`services.gradient.worker.exec.image`.
    #[arg(default_value = "false")]
    pub allow_custom_images: bool,

    /// Whether stopping or restarting the worker stops every guest. The server queues the
    /// exec jobs of stopped guests again without using a retry. `false` keeps guests running
    /// across a worker restart and the restarted worker reattaches to them.
    #[arg(default_value = "true")]
    pub stop_guests_with_worker: bool,

    /// Labels an exec job can ask for. The worker ID is always a label too.
    #[arg(value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Host devices exec jobs can request, a JSON list in the environment variable. Each
    /// entry has a `name` and exactly one source: `vfio` lists PCI addresses bound to
    /// `vfio-pci`, one device each; `dev` names a device node; `usb` holds a `vendor:product`
    /// ID.
    #[arg(value_parser = json_devices, default_value = "[]")]
    pub devices: std::vec::Vec<DeviceConfig>,

    /// Host directories exec jobs can mount at {file}`/mnt/<name>`, a JSON list in the
    /// environment variable. Each entry has a `name`, a `path`, `readOnly` and `perJob`; a
    /// `perJob` mount gives every job a fresh directory below `path`.
    #[arg(value_parser = json_list::<MountConfig>, default_value = "[]")]
    pub mounts: std::vec::Vec<MountConfig>,

    /// Network bridge the guest tap devices join.
    #[arg(default_value = "gradient-guests")]
    pub bridge: String,

    /// IPv4 subnet of {option}`services.gradient.worker.exec.bridge`. The bridge takes the
    /// first host address, each guest the address of its vsock CID.
    #[arg(default_value = "10.233.0.0/16")]
    pub subnet: String,

    /// Path to the `cloud-hypervisor` binary, resolved via `PATH` by default.
    #[arg(default_value = "cloud-hypervisor")]
    pub cloud_hypervisor_bin: String,

    /// Path to the `virtiofsd` binary, resolved via `PATH` by default.
    #[arg(default_value = "virtiofsd")]
    pub virtiofsd_bin: String,

    /// Whether exec jobs may reach the network through
    /// {option}`services.gradient.worker.exec.bridge`.
    #[arg(default_value = "false")]
    pub allow_network: bool,

    /// Whether exec jobs may accept SSH sessions into their guest.
    #[arg(default_value = "false")]
    pub allow_ssh: bool,

    /// Lowest and highest guest port exec jobs may forward, as `low,high`. `null` allows
    /// ports 1024 to 65535.
    #[arg(value_parser = PortRange::from_str)]
    pub allow_ports: Option<PortRange>,

    /// Most nodes an exec job may span.
    #[arg(default_value_t = 1)]
    pub allow_nodes: u32,

    /// Device names from {option}`services.gradient.worker.exec.devices` exec jobs may
    /// request. An empty list allows none.
    #[arg(value_delimiter = ',')]
    pub allow_devices: Vec<String>,

    /// Mount names from {option}`services.gradient.worker.exec.mounts` exec jobs may
    /// request. An empty list allows none.
    #[arg(value_delimiter = ',')]
    pub allow_mounts: Vec<String>,

    /// Most seconds an exec job may run. The server lowers a larger request to this value.
    /// `null` sets no cap.
    pub allow_timeout: Option<u64>,

    /// Most seconds an exec job may go without output. The server lowers a larger request to
    /// this value. `null` sets no cap.
    pub allow_max_silent: Option<u64>,

    /// Most retries an exec job may request.
    #[arg(default_value_t = 0)]
    pub allow_retries: u32,

    /// Whether an exec job that leaves `network` unset reaches the network.
    #[arg(default_value = "false")]
    pub defaults_network: bool,

    /// Whether an exec job that leaves `ssh` unset accepts SSH sessions.
    #[arg(default_value = "false")]
    pub defaults_ssh: bool,

    /// Mounts of an exec job that leaves `mounts` unset.
    #[arg(value_delimiter = ',')]
    pub defaults_mounts: Vec<String>,

    /// Seconds without output after which an exec job that leaves `maxSilent` unset times
    /// out. `null` sets no limit.
    pub defaults_max_silent: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortRange(pub u16, pub u16);

impl FromStr for PortRange {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (low, high) = s
            .split_once(',')
            .ok_or_else(|| format!("{s:?} is not a `low,high` port range"))?;
        let port = |p: &str| {
            p.trim()
                .parse::<u16>()
                .map_err(|e| format!("port {p:?}: {e}"))
        };
        let (low, high) = (port(low)?, port(high)?);
        if low > high {
            return Err(format!("port range {low},{high} is reversed"));
        }

        Ok(Self(low, high))
    }
}

fn json_list<T: DeserializeOwned>(s: &str) -> Result<Vec<T>, String> {
    serde_json::from_str(s).map_err(|e| e.to_string())
}

fn json_devices(s: &str) -> Result<Vec<DeviceConfig>, String> {
    let devices: Vec<DeviceConfig> = json_list(s)?;
    devices.iter().try_for_each(check_device)?;
    Ok(devices)
}
