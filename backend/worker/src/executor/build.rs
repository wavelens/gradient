/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build task - invoke the local nix-daemon to build a single derivation.
//!
//! Unlike the server's `SshBuildExecutor`, the worker builds directly against
//! its own local nix-daemon (no SSH tunneling). Dependencies are already
//! present in the local store (placed there by the server via NarPush or S3).
//!
//! The build pipeline is encoded as a type-state chain:
//!
//! ```text
//! ParsedDerivation::load(drv_path)   →  ParsedDerivation
//!                     .realize(…)    →  Vec<BuildOutput>
//! ```
//!
//! `build_derivation` is a thin orchestrator that threads these stages
//! together and reports the result to the server.

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::StreamExt as _;
use gradient_core::db::{DrvOutputSpec, parse_drv};
use gradient_core::executer::path_utils::{nix_store_path, strip_nix_store_prefix};
use gradient_core::hydra::parse_hydra_product_line;
use gradient_core::sources::get_hash_from_path;
use harmonia_protocol::daemon_wire::types2::{BuildMode, BuildResultInner};
use harmonia_protocol::log::{Field, LogMessage, ResultType, Verbosity};
use harmonia_protocol::types::ClientOptions;
use harmonia_store_content_address::{ContentAddress, ContentAddressMethod};
use harmonia_store_derivation::derivation::{BasicDerivation, DerivationOutput, DerivationT};
use harmonia_store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use harmonia_utils_hash::{Algorithm, Hash};
use proto::messages::{BuildFailureKind, BuildMetrics, BuildOutput, BuildProduct, BuildTask};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::{Pin, pin};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::metrics::cgroup::{BuildMetricsRaw, read_build_cgroup};
use crate::nix::store::{LocalNixStore, strip_store_prefix};
use crate::proto::job::JobUpdater;

const BYTES_PER_MB: u64 = 1_048_576;
/// Bounded depth for the best-effort cgroup search under the cgroup root.
const CGROUP_SEARCH_DEPTH: usize = 4;

// ── Failure classification ────────────────────────────────────────────────────

/// A build failure carrying its classification, so the dispatch layer can
/// report the right `BuildFailureKind` to the server.
#[derive(Debug)]
pub struct BuildError {
    pub kind: BuildFailureKind,
    pub source: anyhow::Error,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.source)
    }
}
impl std::error::Error for BuildError {}

impl BuildError {
    pub(crate) fn transient(e: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: BuildFailureKind::Transient,
            source: e.into(),
        }
    }
    pub(crate) fn permanent(e: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: BuildFailureKind::Permanent,
            source: e.into(),
        }
    }
    pub(crate) fn timeout(e: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: BuildFailureKind::Timeout,
            source: e.into(),
        }
    }
    /// The server sent `AbortJob` while the daemon was building. Terminal: the
    /// build is already in a terminal state server-side, so retrying is wrong.
    pub(crate) fn aborted(drv_path: &str) -> Self {
        Self {
            kind: BuildFailureKind::Permanent,
            source: anyhow::anyhow!("build aborted by server: {}", drv_path),
        }
    }
}

/// Best-effort OOM signature scan. OOM presents as a generic build failure but
/// is transient (retry on a less-loaded builder).
pub(super) fn looks_like_oom(msg: &str) -> bool {
    let l = msg.to_ascii_lowercase();
    l.contains("out of memory")
        || l.contains("cannot allocate memory")
        || l.contains("oom-killer")
        || l.contains("killed")
}

/// Classify a builder-reported failure message: OOM -> Transient, otherwise a
/// real build error -> Permanent.
pub(super) fn classify_build_error(msg: &str) -> BuildFailureKind {
    if looks_like_oom(msg) {
        BuildFailureKind::Transient
    } else {
        BuildFailureKind::Permanent
    }
}

// ── Build metrics ───────────────────────────────────────────────────────────

/// Convert a raw cgroup snapshot into the wire `BuildMetrics`.
///
/// `build_time_ms` is always reported. Cgroup-derived fields are `None` when
/// `raw` is absent. `avg_cpu_pct` is `None` when it cannot be computed
/// (zero build time or zero CPU count) to avoid divide-by-zero.
fn raw_to_build_metrics(
    raw: Option<BuildMetricsRaw>,
    build_time_ms: u64,
    cpu_count: u32,
) -> BuildMetrics {
    let Some(raw) = raw else {
        return BuildMetrics {
            build_time_ms: Some(build_time_ms),
            ..Default::default()
        };
    };

    let cpu_time_ms = raw.cpu_usage_usec.map(|u| u / 1000);
    let avg_cpu_pct = match cpu_time_ms {
        Some(cpu_ms) if build_time_ms > 0 && cpu_count > 0 => {
            Some(cpu_ms as f32 / (build_time_ms as f32 * cpu_count as f32) * 100.0)
        }
        _ => None,
    };

    BuildMetrics {
        peak_ram_mb: raw.peak_ram_bytes.map(|b| b / BYTES_PER_MB),
        cpu_time_ms,
        avg_cpu_pct,
        disk_read_bytes: Some(raw.disk_read_bytes),
        disk_write_bytes: Some(raw.disk_write_bytes),
        oom_killed: raw.oom_killed,
        build_time_ms: Some(build_time_ms),
    }
}

/// Best-effort search for the cgroup directory of a daemon-forked build.
///
/// Nix's experimental `use-cgroups` feature places each build in its own
/// cgroup whose leaf name embeds the derivation hash. Mapping a build to its
/// cgroup is environment-specific, so this walks at most a few levels under
/// `root` and returns the first directory whose name contains the drv hash.
/// Returns `None` when nothing matches.
fn locate_build_cgroup(root: &Path, drv_path: &str) -> Option<PathBuf> {
    let hash = drv_hash(drv_path)?;
    find_dir_containing(root, &hash, CGROUP_SEARCH_DEPTH)
}

/// Extract the store-path hash from a `.drv` path (`/nix/store/<hash>-name.drv`).
fn drv_hash(drv_path: &str) -> Option<String> {
    let base = drv_path.rsplit('/').next().unwrap_or(drv_path);
    let hash = base.split('-').next()?;
    (!hash.is_empty()).then(|| hash.to_owned())
}

/// Bounded breadth-first walk returning the first directory whose name contains
/// `needle`. Never follows symlinks; never recurses past `max_depth`.
fn find_dir_containing(root: &Path, needle: &str, max_depth: usize) -> Option<PathBuf> {
    let mut frontier = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = frontier.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if entry.file_name().to_string_lossy().contains(needle) {
                return Some(path);
            }
            if depth + 1 < max_depth {
                frontier.push((path, depth + 1));
            }
        }
    }
    None
}

/// Capture per-build resource metrics. Always reports `build_time_ms`; cgroup
/// fields degrade to `None` when metrics are disabled or the cgroup cannot be
/// located/read.
fn capture_build_metrics(
    enabled: bool,
    cgroup_root: &str,
    drv_path: &str,
    build_time_ms: u64,
) -> BuildMetrics {
    let cpu_count = crate::metrics::host_static().cpu_count;
    if !enabled {
        return raw_to_build_metrics(None, build_time_ms, cpu_count);
    }
    let raw = locate_build_cgroup(Path::new(cgroup_root), drv_path)
        .and_then(|dir| read_build_cgroup(&dir));
    if raw.is_none() {
        debug!(drv = %drv_path, "build cgroup not found; reporting wall-clock time only");
    }
    raw_to_build_metrics(raw, build_time_ms, cpu_count)
}

// ── Type-state pipeline ───────────────────────────────────────────────────────

/// A `.drv` file read from disk and parsed into all data needed to call
/// `DaemonStore::build_derivation`.
///
/// Obtain via [`ParsedDerivation::load`]; advance to built outputs via
/// [`ParsedDerivation::realize`].
pub(super) struct ParsedDerivation {
    drv: gradient_core::db::Derivation,
    harmonia_path: StorePath,
    basic_drv: BasicDerivation,
}

impl ParsedDerivation {
    /// Read and parse a `.drv` file from the local Nix store.
    pub(super) async fn load(drv_path: &str) -> Result<Self> {
        let path = nix_store_path(drv_path);
        debug!(drv = %path, "building derivation locally");

        let drv_bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("read .drv file: {}", path))?;

        let drv = parse_drv(&drv_bytes).with_context(|| format!("parse .drv file: {}", path))?;

        let harmonia_path = StorePath::from_base_path(strip_store_prefix(&path))
            .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", path, e))?;

        let basic_drv = get_basic_derivation(&path, &drv)?;

        Ok(Self {
            drv,
            harmonia_path,
            basic_drv,
        })
    }

    /// Submit this derivation to the local nix-daemon and collect the realised
    /// outputs.
    ///
    /// Streams build log lines to the server via `updater` while the daemon is
    /// running.  Returns one [`BuildOutput`] per output name; `nar_size` and
    /// `nar_hash` are `None` at this stage and are filled in by the compress
    /// step.
    pub(super) async fn realize(
        self,
        store: &LocalNixStore,
        task_index: u32,
        updater: &mut JobUpdater,
        drv_path: &str,
        max_silent_secs: Option<u64>,
        abort: &mut watch::Receiver<bool>,
    ) -> Result<Vec<BuildOutput>, BuildError> {
        let mut guard = store.scoped().await.map_err(BuildError::transient)?;

        debug!(
            drv = %drv_path,
            platform = %self.drv.system,
            builder = %self.drv.builder,
            outputs = ?self.drv.outputs.iter().map(|o| &o.name).collect::<Vec<_>>(),
            input_drvs = self.drv.input_derivations.len(),
            input_srcs = self.drv.input_sources.len(),
            env_keys = ?self.drv.environment.keys().collect::<Vec<_>>(),
            "sending BasicDerivation to nix-daemon"
        );

        let mut opts = ClientOptions::default();
        opts.verbose_build = Verbosity::Talkative;
        opts.verbosity = Verbosity::Notice;
        opts.use_substitutes = false;
        opts.build_cores = 0;

        if let Err(e) = guard.client().set_options(&opts).await {
            warn!(error = %e, "set_options failed; discarding daemon connection");
            return Err(BuildError::transient(anyhow::anyhow!(
                "set_options failed for {}: {}",
                drv_path,
                e
            )));
        }

        // `build_derivation` returns `impl ResultLog = Stream<Item=LogMessage> + Future`.
        // Drain the stream first, then await the future for the BuildResult.
        //
        // On abort we return early *without* `mark_ok`: dropping `logs` and
        // then `guard` discards the daemon connection (see `ScopedGuard`),
        // which closes the socket and makes the nix-daemon kill the in-flight
        // build instead of leaving it compiling after the server cancelled.
        let silent = max_silent_secs.map(std::time::Duration::from_secs);
        let outcome = {
            let logs = guard.client().build_derivation(
                &self.harmonia_path,
                &self.basic_drv,
                BuildMode::Normal,
            );
            let mut logs = pin!(logs);
            match drain_build_logs_with_timeout(logs.as_mut(), updater, task_index, silent, abort)
                .await
            {
                Ok(DrainOutcome::Completed(stats)) => log_stream_summary(&stats, drv_path),
                Ok(DrainOutcome::Aborted) => return Err(BuildError::aborted(drv_path)),
                Err(e) => return Err(BuildError::timeout(e)),
            }
            logs.await
        };

        let result = match outcome {
            Ok(r) => {
                guard.mark_ok();
                r
            }
            Err(e) => {
                return Err(BuildError::transient(anyhow::anyhow!(
                    "build_derivation failed for {}: {}",
                    drv_path,
                    e
                )));
            }
        };

        match result.inner {
            BuildResultInner::Success(s) => {
                info!(drv = %drv_path, "build succeeded");
                let pairs = output_pairs_from_built_or_drv(&s.built_outputs, &self.drv);
                if pairs.is_empty() {
                    return Err(BuildError::permanent(anyhow::anyhow!(
                        "build of {} reported success but produced no recordable outputs - \
                         the daemon returned no built_outputs and the .drv carries no \
                         input-addressed paths to recover (likely a content-addressed or \
                         deferred-output derivation built against an old protocol)",
                        drv_path
                    )));
                }
                if s.built_outputs.is_empty() {
                    debug!(
                        drv = %drv_path,
                        recovered = pairs.len(),
                        "daemon returned empty built_outputs (output already valid or legacy \
                         protocol); recovering input-addressed/FOD paths from .drv"
                    );
                }
                let mut outputs = Vec::with_capacity(pairs.len());
                for (output_name, store_path_str) in pairs {
                    let (hash, _package) = get_hash_from_path(store_path_str.clone())
                        .with_context(|| format!("parse output path: {}", store_path_str))
                        .map_err(BuildError::permanent)?;
                    let products = load_products(&store_path_str).await;
                    outputs.push(BuildOutput {
                        name: output_name,
                        store_path: store_path_str,
                        hash,
                        nar_size: None,
                        nar_hash: None,
                        products,
                    });
                }
                Ok(outputs)
            }

            BuildResultInner::Failure(f) => {
                let msg = String::from_utf8_lossy(&f.error_msg).to_string();
                warn!(drv = %drv_path, error = %msg, "build failed");
                let kind = classify_build_error(&msg);
                Err(BuildError {
                    kind,
                    source: anyhow::anyhow!("build failed: {}", msg),
                })
            }
        }
    }
}

/// Resolve `(output_name, full_store_path)` pairs for a successful build.
///
/// The daemon's `built_outputs` is authoritative when populated - for
/// content-addressed or deferred-output drvs the realised path is only
/// knowable post-build.
///
/// When `built_outputs` is empty (the daemon reported success but didn't
/// emit a new realisation, e.g. the path was already valid for a fixed-output
/// derivation, or the negotiated protocol predates
/// `realisation-with-path-not-hash` and harmonia's deserializer dropped the
/// map), we fall back to the parsed `.drv`'s declared output paths.
/// Input-addressed drvs and FODs already carry the exact path, so the
/// recovery is correct for them. Empty `path` entries (true CA / deferred
/// outputs) are skipped - the caller fails the build when no pairs survive,
/// since a CA build with no daemon-reported realisation cannot be recorded
/// safely.
fn output_pairs_from_built_or_drv(
    built_outputs: &BTreeMap<
        harmonia_store_derivation::derived_path::OutputName,
        harmonia_store_derivation::realisation::UnkeyedRealisation,
    >,
    drv: &gradient_core::db::Derivation,
) -> Vec<(String, String)> {
    if !built_outputs.is_empty() {
        return built_outputs
            .iter()
            .map(|(name, real)| (name.to_string(), format!("/nix/store/{}", real.out_path)))
            .collect();
    }
    drv.outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| (o.name.clone(), o.path.clone()))
        .collect()
}

// ── Hydra product loader ──────────────────────────────────────────────────────

/// Read and parse `nix-support/hydra-build-products` from `store_path`, returning
/// one [`BuildProduct`] per valid line. Returns an empty vec if the file is absent.
pub(super) async fn load_products(store_path: &str) -> Vec<BuildProduct> {
    let file_path = format!("{}/nix-support/hydra-build-products", store_path);
    let Ok(content) = tokio::fs::read_to_string(&file_path).await else {
        return Vec::new();
    };
    let mut products = Vec::new();
    for line in content.lines() {
        if let Some((file_type, subtype, path)) = parse_hydra_product_line(line) {
            let name = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| path.clone());
            let size = tokio::fs::metadata(&path).await.ok().map(|m| m.len());
            products.push(BuildProduct {
                file_type,
                subtype,
                name,
                path,
                size,
            });
        }
    }
    products
}

// ── Orchestrator ──────────────────────────────────────────────────────────────

/// Build a single derivation on the local nix-daemon.
///
/// Reports [`JobUpdateKind::Building`] at start and
/// [`JobUpdateKind::BuildOutput`] with the realised outputs on success.
/// Streams build log lines to the server via `LogChunk` messages while the
/// daemon is running.
#[allow(clippy::too_many_arguments)]
pub async fn build_derivation(
    store: &LocalNixStore,
    task: &BuildTask,
    task_index: u32,
    updater: &mut JobUpdater,
    abort: &mut watch::Receiver<bool>,
    build_metrics: bool,
    cgroup_root: &str,
) -> Result<Vec<BuildOutput>, BuildError> {
    // `report_building` is sent by the caller (`execute_build_job`) before the
    // prefetch step, so a `JobFailed` after a prefetch error finds the build
    // already in `Building` state on the server.

    let parsed = ParsedDerivation::load(&task.drv_path)
        .await
        .map_err(BuildError::transient)?;

    let realize = parsed.realize(
        store,
        task_index,
        updater,
        &task.drv_path,
        task.max_silent_secs,
        abort,
    );

    let started = std::time::Instant::now();
    let realize_result: Result<Vec<BuildOutput>, BuildError> =
        match task.timeout_secs.map(std::time::Duration::from_secs) {
            Some(d) => match tokio::time::timeout(d, realize).await {
                Ok(r) => r,
                Err(_) => Err(BuildError::timeout(anyhow::anyhow!(
                    "build exceeded wall-clock timeout of {}s",
                    d.as_secs()
                ))),
            },
            None => realize.await,
        };

    // Capture metrics (incl. wall-clock time) for this build and ship them
    // inline with its `BuildOutput`. On failure they are dropped with the
    // error; the scheduler records metrics only for completed builds.
    let build_time_ms = started.elapsed().as_millis() as u64;
    let metrics = capture_build_metrics(build_metrics, cgroup_root, &task.drv_path, build_time_ms);

    let outputs = realize_result?;
    updater
        .report_build_output(task.build_id.clone(), outputs.clone(), Some(metrics))
        .map_err(BuildError::transient)?;
    Ok(outputs)
}

// ── Log helpers ───────────────────────────────────────────────────────────────

/// Counters collected while draining the harmonia build log stream.
#[derive(Default)]
struct LogStreamStats {
    total_msgs: u64,
    forwarded_lines: u64,
    forwarded_bytes: u64,
    send_failures: u64,
}

/// Why the build log drain stopped.
enum DrainOutcome {
    /// The daemon finished the build and closed the log stream normally.
    Completed(LogStreamStats),
    /// The server signalled `AbortJob` mid-build; the caller must drop the
    /// daemon connection so the build is killed.
    Aborted,
}

/// One step of the build log stream: the next message, end-of-stream, or a
/// server abort.
enum NextLog {
    Message(LogMessage),
    StreamEnd,
    Aborted,
}

/// Resolve the next event on the build log stream, racing it against the
/// server abort signal and the `maxSilent` budget.
///
/// Returns [`NextLog::Aborted`] the instant `abort` is set so the caller can
/// tear down the daemon connection and let the daemon kill the running build.
/// Returns `Err` only when `silent` elapses with no new log line.
async fn next_log_event<S>(
    mut logs: Pin<&mut S>,
    silent: Option<std::time::Duration>,
    abort: &mut watch::Receiver<bool>,
) -> Result<NextLog>
where
    S: futures::Stream<Item = LogMessage>,
{
    if *abort.borrow() {
        return Ok(NextLog::Aborted);
    }
    tokio::select! {
        biased;
        _ = abort.changed() => Ok(NextLog::Aborted),
        item = logs.next() => Ok(match item {
            Some(msg) => NextLog::Message(msg),
            None => NextLog::StreamEnd,
        }),
        _ = maybe_silent_timeout(silent) => Err(anyhow::anyhow!(
            "build produced no output for {}s (maxSilent exceeded)",
            silent.map(|d| d.as_secs()).unwrap_or_default()
        )),
    }
}

/// Sleep for the `maxSilent` budget, or never resolve when no budget is set.
async fn maybe_silent_timeout(silent: Option<std::time::Duration>) {
    match silent {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// Drain every [`LogMessage`] from `logs`, forwarding text lines to `updater`.
///
/// Stops early with [`DrainOutcome::Aborted`] when the server cancels the job,
/// or with `Err` when no log line arrives within `silent` (the build's
/// `maxSilent` budget; `None` disables it).
async fn drain_build_logs_with_timeout<S>(
    mut logs: Pin<&mut S>,
    updater: &mut JobUpdater,
    task_index: u32,
    silent: Option<std::time::Duration>,
    abort: &mut watch::Receiver<bool>,
) -> Result<DrainOutcome>
where
    S: futures::Stream<Item = LogMessage>,
{
    let mut stats = LogStreamStats::default();
    loop {
        let msg = match next_log_event(logs.as_mut(), silent, abort).await? {
            NextLog::Message(msg) => msg,
            NextLog::StreamEnd => break,
            NextLog::Aborted => return Ok(DrainOutcome::Aborted),
        };
        stats.total_msgs += 1;
        if let Some(line) = log_message_to_text(&msg) {
            let len = line.len();
            // Log streaming is best-effort - never fail the build because the
            // server connection hiccupped.
            match updater.send_log_chunk(task_index, line.into_bytes()) {
                Ok(()) => {
                    stats.forwarded_lines += 1;
                    stats.forwarded_bytes += len as u64;
                }

                Err(e) => {
                    stats.send_failures += 1;
                    warn!(error = %e, "failed to forward build log chunk; continuing");
                }
            }
        }
    }
    Ok(DrainOutcome::Completed(stats))
}

/// Emit a tracing summary after [`drain_build_logs_with_timeout`] completes.
///
/// Warns if the daemon emitted no messages at all, or if it emitted messages
/// but none were forwardable text (suggesting daemon verbosity is too low).
fn log_stream_summary(stats: &LogStreamStats, drv_path: &str) {
    info!(
        drv = %drv_path,
        daemon_messages = stats.total_msgs,
        forwarded_lines = stats.forwarded_lines,
        forwarded_bytes = stats.forwarded_bytes,
        send_failures = stats.send_failures,
        "build log stream drained"
    );

    if stats.total_msgs == 0 {
        warn!(
            drv = %drv_path,
            "daemon emitted zero LogMessages during build - daemon verbosity may be too low \
             (set `verbose-builds = true` and `log-lines = 0` in nix.conf, or check \
             the worker user's permissions to read daemon output)"
        );
    } else if stats.forwarded_lines == 0 {
        warn!(
            drv = %drv_path,
            daemon_messages = stats.total_msgs,
            "daemon emitted LogMessages but none had forwardable text content \
             (only structured progress / activity events) - set `verbose-builds = true` \
             on the worker's nix-daemon to enable BuildLogLine results"
        );
    }
}

/// Extract a forwardable log line from a harmonia daemon log message.
///
/// Captures:
/// - `Message`: high-level messages (errors, warnings, status notes).
/// - `StartActivity`: activity descriptions ("building '/nix/store/…'",
///   "copying '/nix/store/…'" etc.). Useful for builtins (fetchurl, path)
///   that run inside the daemon rather than in a sandbox and therefore never
///   produce `BuildLogLine` results.
/// - `BuildLogLine`/`PostBuildLogLine` results: the raw stdout/stderr lines
///   from the build sandbox or post-build hook (the actual build log).
///
/// `StopActivity`, `Progress`, `SetExpected`, `SetPhase`, and other
/// structured result types are skipped - they're progress-bar bookkeeping,
/// not user-facing log content.
fn log_message_to_text(msg: &LogMessage) -> Option<String> {
    match msg {
        LogMessage::Message(m) => {
            let s = String::from_utf8_lossy(&m.text);
            if s.is_empty() {
                return None;
            }
            Some(format!("{s}\n"))
        }

        LogMessage::StartActivity(a) => {
            let s = String::from_utf8_lossy(&a.text);
            if s.is_empty() {
                return None;
            }
            Some(format!("{s}\n"))
        }

        LogMessage::Result(r)
            if matches!(
                r.result_type,
                ResultType::BuildLogLine | ResultType::PostBuildLogLine
            ) =>
        {
            // BuildLogLine/PostBuildLogLine results carry the line as the first String field.
            r.fields.iter().find_map(|f| match f {
                Field::String(b) => {
                    let s = String::from_utf8_lossy(b);
                    if s.is_empty() {
                        return None;
                    }
                    Some(format!("{s}\n"))
                }
                _ => None,
            })
        }
        _ => None,
    }
}

// ── Derivation construction ───────────────────────────────────────────────────

/// Construct a harmonia [`BasicDerivation`] from a parsed drv file.
///
/// Output paths are taken directly from the `.drv` file:
/// - non-empty `path` → `InputAddressed` (concrete store path)
/// - empty `path` → `Deferred` (floating CA derivation)
///
/// This avoids calling `query_derivation_output_map`, which fails on some
/// daemon versions that return full `/nix/store/...` paths where harmonia
/// expects bare `hash-name` paths.
///
/// Structured attributes (`__json`) are moved from the env map to
/// `structured_attrs` so the daemon handles them correctly.
fn get_basic_derivation(
    full_drv_path: &str,
    drv: &gradient_core::db::Derivation,
) -> Result<BasicDerivation> {
    // ── Build outputs from .drv data ──────────────────────────────────────────
    //
    // Three shapes of `.drv` output to disambiguate:
    //
    // 1. Fixed-output derivation (FOD): both `hash_algo` and `hash` are
    //    populated (e.g. a `fetchurl`). The daemon **must** see this as
    //    `CAFixed(ContentAddress)` because that's what unlocks network
    //    access in the build sandbox - passing it as `InputAddressed`
    //    sandboxes it without DNS, so curl fails with
    //    `Could not resolve host: …` and the build dies.
    // 2. Floating CA derivation: `path` is empty AND `hash_algo` is empty.
    //    Daemon will compute the path from the build output → `Deferred`.
    // 3. Plain input-addressed derivation: `path` is set, no `hash_algo`.
    //    → `InputAddressed(StorePath)`.
    let mut outputs: BTreeMap<_, _> = BTreeMap::new();
    for o in &drv.outputs {
        let output_name = o
            .name
            .parse()
            .with_context(|| format!("invalid output name '{}' in {}", o.name, full_drv_path))?;

        let drv_output = match o.as_spec() {
            DrvOutputSpec::FixedOutput { hash_algo, hash } => ca_fixed_output(hash_algo, hash)
                .with_context(|| {
                    format!(
                        "invalid FOD spec for output '{}' in {} (hash_algo={:?} hash={:?})",
                        o.name, full_drv_path, hash_algo, hash
                    )
                })?,
            DrvOutputSpec::Deferred => DerivationOutput::Deferred,
            DrvOutputSpec::InputAddressed { path } => {
                let base = strip_nix_store_prefix(path);
                let sp = StorePath::from_base_path(&base).with_context(|| {
                    format!("invalid output path '{}' in {}", path, full_drv_path)
                })?;
                DerivationOutput::InputAddressed(sp)
            }
        };

        outputs.insert(output_name, drv_output);
    }

    // ── Input paths: input_sources + output paths of input_derivations ────────
    // The daemon needs all direct inputs present in the store before building.
    // input_sources are plain store paths; input_derivations map drv→outputs,
    // so we read each input .drv to resolve the concrete output paths.
    let mut inputs: harmonia_store_path::StorePathSet = drv
        .input_sources
        .iter()
        .filter_map(|p| {
            let full = nix_store_path(p);
            let base = strip_nix_store_prefix(&full).to_owned();
            match StorePath::from_base_path(&base) {
                Ok(sp) => Some(sp),
                Err(e) => {
                    warn!(path = %p, error = %e, "skipping input_src: not a valid store path");
                    None
                }
            }
        })
        .collect();

    for (input_drv_path, _output_names) in &drv.input_derivations {
        let input_full = nix_store_path(input_drv_path);
        let input_bytes = match std::fs::read(&input_full) {
            Ok(b) => b,
            Err(e) => {
                warn!(drv = %input_full, error = %e, "cannot read input .drv for inputs");
                continue;
            }
        };

        let input_drv = match parse_drv(&input_bytes) {
            Ok(d) => d,
            Err(e) => {
                warn!(drv = %input_full, error = %e, "cannot parse input .drv for inputs");
                continue;
            }
        };

        for o in &input_drv.outputs {
            if o.path.is_empty() {
                continue;
            }

            let base = strip_nix_store_prefix(&o.path);
            match StorePath::from_base_path(&base) {
                Ok(sp) => {
                    inputs.insert(sp);
                }

                Err(e) => {
                    warn!(path = %o.path, error = %e, "skipping input drv output: not a valid store path");
                }
            }
        }
    }

    // ── Structured attributes ─────────────────────────────────────────────────
    // harmonia's NixSerialize for BasicDerivation never writes `structured_attrs`
    // to the wire - only `env` is sent. The `__json` key in env is what the Nix
    // daemon reads for structured-attrs derivations, so leave it in place.

    // Extract the name from the drv path ("hash-name.drv" → "name.drv").
    let base = strip_nix_store_prefix(full_drv_path);
    let drv_name = base
        .find('-')
        .map(|i| base[i + 1..].to_owned())
        .unwrap_or_else(|| base.to_owned());

    Ok(DerivationT {
        name: drv_name
            .parse()
            .with_context(|| format!("invalid derivation name: {}", drv_name))?,
        outputs,
        inputs,
        platform: Bytes::from(drv.system.clone()),
        builder: Bytes::from(drv.builder.clone()),
        args: drv.args.iter().map(|a| Bytes::from(a.clone())).collect(),
        env: drv
            .environment
            .iter()
            .map(|(k, v)| (Bytes::from(k.clone()), Bytes::from(v.clone())))
            .collect(),
        structured_attrs: None,
    })
}

/// Build a `DerivationOutput::CAFixed(...)` from a `.drv`'s `outputHashAlgo`
/// and `outputHash` fields. Without this the daemon would treat the FOD as
/// an input-addressed derivation, sandbox it without network access, and
/// every fetch (curl, git clone, …) would fail with DNS errors.
///
/// The `.drv` `hash_algo` field follows Nix's wire format:
///   `"sha256"`        → flat sha256
///   `"r:sha256"`      → recursive (NAR-hashed) sha256
///   `"text:sha256"`   → text-hashed (rare, used by `builtins.toFile`)
///
/// The `hash` field is hex-encoded (base16) raw digest bytes.
fn ca_fixed_output(hash_algo: &str, hash_hex: &str) -> Result<DerivationOutput> {
    let (method, algo_str) = if let Some(rest) = hash_algo.strip_prefix("r:") {
        (ContentAddressMethod::NixArchive, rest)
    } else if let Some(rest) = hash_algo.strip_prefix("text:") {
        (ContentAddressMethod::Text, rest)
    } else {
        (ContentAddressMethod::Flat, hash_algo)
    };

    let algorithm: Algorithm = algo_str
        .parse()
        .map_err(|e| anyhow::anyhow!("unknown hash algorithm {:?}: {}", algo_str, e))?;

    let hash_bytes = hex::decode(hash_hex)
        .with_context(|| format!("hash field {:?} is not valid hex", hash_hex))?;
    let hash = Hash::from_slice(algorithm, &hash_bytes).with_context(|| {
        format!(
            "hash length {} doesn't match {:?} digest size",
            hash_bytes.len(),
            algorithm
        )
    })?;

    let ca = ContentAddress::from_hash(method, hash)
        .map_err(|e| anyhow::anyhow!("ContentAddress::from_hash failed: {}", e))?;
    Ok(DerivationOutput::CAFixed(ca))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_fixed_flat_sha256() {
        // sha256("hello") in hex
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let out = ca_fixed_output("sha256", h).unwrap();
        match out {
            DerivationOutput::CAFixed(ca) => {
                assert_eq!(ca.method(), ContentAddressMethod::Flat);
                assert_eq!(ca.algorithm(), Algorithm::SHA256);
            }
            other => panic!("expected CAFixed(Flat), got {other:?}"),
        }
    }

    #[test]
    fn ca_fixed_recursive_sha256() {
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let out = ca_fixed_output("r:sha256", h).unwrap();
        match out {
            DerivationOutput::CAFixed(ca) => {
                assert_eq!(ca.method(), ContentAddressMethod::NixArchive);
            }
            other => panic!("expected CAFixed(NixArchive), got {other:?}"),
        }
    }

    #[test]
    fn ca_fixed_text_sha256() {
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let out = ca_fixed_output("text:sha256", h).unwrap();
        assert!(matches!(out, DerivationOutput::CAFixed(_)));
    }

    #[test]
    fn ca_fixed_rejects_garbage_algo() {
        assert!(ca_fixed_output("blake7", "deadbeef").is_err());
    }

    #[test]
    fn ca_fixed_rejects_bad_hex() {
        assert!(ca_fixed_output("sha256", "not-hex").is_err());
    }

    #[test]
    fn ca_fixed_rejects_wrong_length_hash() {
        // sha256 needs 32 bytes (64 hex chars); pass 8 bytes (16 hex chars).
        assert!(ca_fixed_output("sha256", "deadbeefdeadbeef").is_err());
    }

    fn drv_with_outputs(outputs: Vec<(&str, &str)>) -> gradient_core::db::Derivation {
        gradient_core::db::Derivation {
            outputs: outputs
                .into_iter()
                .map(|(name, path)| gradient_core::db::DerivationOutput {
                    name: name.to_string(),
                    path: path.to_string(),
                    hash_algo: String::new(),
                    hash: String::new(),
                })
                .collect(),
            input_derivations: vec![],
            input_sources: vec![],
            system: String::new(),
            builder: String::new(),
            args: vec![],
            environment: std::collections::HashMap::new(),
        }
    }

    fn realisation(out_path: &str) -> harmonia_store_derivation::realisation::UnkeyedRealisation {
        let base = out_path.strip_prefix("/nix/store/").unwrap_or(out_path);
        harmonia_store_derivation::realisation::UnkeyedRealisation {
            out_path: harmonia_store_path::StorePath::from_base_path(base).unwrap(),
            signatures: std::collections::BTreeSet::new(),
        }
    }

    #[test]
    fn output_pairs_use_built_outputs_when_daemon_returned_them() {
        let mut built = BTreeMap::new();
        built.insert(
            "out".parse().unwrap(),
            realisation("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo"),
        );
        // Drv path differs - must be ignored when built_outputs is non-empty
        // (the daemon's realisation is canonical for CA / FOD outputs).
        let drv = drv_with_outputs(vec![(
            "out",
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-foo",
        )]);

        let pairs = output_pairs_from_built_or_drv(&built, &drv);
        assert_eq!(
            pairs,
            vec![(
                "out".to_string(),
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()
            )]
        );
    }

    #[test]
    fn output_pairs_recover_from_drv_when_built_outputs_empty() {
        // Either an old-protocol daemon (harmonia drained the legacy map) or a
        // modern daemon that emits success without a fresh realisation
        // (e.g. FOD output already valid). For input-addressed and FOD drvs
        // the .drv carries the path; recover it.
        let built = BTreeMap::new();
        let drv = drv_with_outputs(vec![
            ("out", "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo"),
            ("dev", "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-foo-dev"),
        ]);

        let mut pairs = output_pairs_from_built_or_drv(&built, &drv);
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (
                    "dev".to_string(),
                    "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-foo-dev".to_string()
                ),
                (
                    "out".to_string(),
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()
                ),
            ]
        );
    }

    #[test]
    fn output_pairs_skip_drv_outputs_with_empty_path() {
        // CA / deferred outputs carry an empty `path` until the daemon emits
        // a realisation. With nothing to fall back on for those, only the
        // input-addressed siblings survive - and `realize` errors out when
        // the result is empty so a CA-only build doesn't go silently
        // undocumented.
        let built = BTreeMap::new();
        let drv = drv_with_outputs(vec![
            ("out", "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo"),
            ("ca-out", ""),
        ]);

        let pairs = output_pairs_from_built_or_drv(&built, &drv);
        assert_eq!(
            pairs,
            vec![(
                "out".to_string(),
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string()
            )]
        );
    }

    #[test]
    fn output_pairs_returns_empty_for_pure_ca_drv_without_realisation() {
        // No daemon realisation, .drv has only CA / deferred outputs (empty
        // path). Recovery cannot produce any pairs; the caller's
        // is_empty() check turns this into a build failure.
        let built = BTreeMap::new();
        let drv = drv_with_outputs(vec![("out", ""), ("dev", "")]);

        let pairs = output_pairs_from_built_or_drv(&built, &drv);
        assert!(pairs.is_empty());
    }

    use tokio::sync::watch;

    #[tokio::test]
    async fn next_log_event_returns_aborted_when_already_set() {
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        let stream = futures::stream::pending::<LogMessage>();
        let mut stream = std::pin::pin!(stream);
        let out = next_log_event(stream.as_mut(), None, &mut rx).await;
        assert!(matches!(out, Ok(NextLog::Aborted)));
    }

    #[tokio::test]
    async fn next_log_event_aborts_while_waiting_on_stalled_stream() {
        let (tx, mut rx) = watch::channel(false);
        let stream = futures::stream::pending::<LogMessage>();
        let mut stream = std::pin::pin!(stream);
        let mut fut = std::pin::pin!(next_log_event(stream.as_mut(), None, &mut rx));
        assert!(futures::poll!(fut.as_mut()).is_pending());
        tx.send(true).unwrap();
        assert!(matches!(fut.await, Ok(NextLog::Aborted)));
    }

    #[tokio::test]
    async fn next_log_event_reports_stream_end() {
        let (_tx, mut rx) = watch::channel(false);
        let stream = futures::stream::empty::<LogMessage>();
        let mut stream = std::pin::pin!(stream);
        let out = next_log_event(stream.as_mut(), None, &mut rx).await;
        assert!(matches!(out, Ok(NextLog::StreamEnd)));
    }

    #[tokio::test(start_paused = true)]
    async fn next_log_event_errors_on_silent_timeout() {
        let (_tx, mut rx) = watch::channel(false);
        let stream = futures::stream::pending::<LogMessage>();
        let mut stream = std::pin::pin!(stream);
        let mut fut = std::pin::pin!(next_log_event(
            stream.as_mut(),
            Some(std::time::Duration::from_secs(5)),
            &mut rx,
        ));
        assert!(futures::poll!(fut.as_mut()).is_pending());
        tokio::time::advance(std::time::Duration::from_secs(6)).await;
        assert!(fut.await.is_err());
    }

    #[test]
    fn locate_build_cgroup_none_for_empty_root() {
        let dir = tempfile::tempdir().unwrap();
        let found = locate_build_cgroup(
            dir.path(),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv",
        );
        assert!(found.is_none());
    }

    #[test]
    fn locate_build_cgroup_finds_dir_with_hash() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let nested = dir.path().join("system.slice").join(format!("nix-{hash}-foo.scope"));
        std::fs::create_dir_all(&nested).unwrap();
        let found = locate_build_cgroup(
            dir.path(),
            &format!("/nix/store/{hash}-foo.drv"),
        );
        assert_eq!(found, Some(nested));
    }

    #[test]
    fn raw_to_metrics_always_sets_build_time() {
        let m = raw_to_build_metrics(None, 5_000, 4);
        assert_eq!(m.build_time_ms, Some(5_000));
        assert_eq!(m.peak_ram_mb, None);
        assert_eq!(m.cpu_time_ms, None);
        assert_eq!(m.avg_cpu_pct, None);
        assert!(!m.oom_killed);
    }

    #[test]
    fn raw_to_metrics_handles_zero_divisors() {
        let raw = BuildMetricsRaw {
            peak_ram_bytes: Some(2 * BYTES_PER_MB),
            cpu_usage_usec: Some(1_000_000),
            disk_read_bytes: 10,
            disk_write_bytes: 20,
            oom_killed: false,
        };
        // build_time_ms = 0 → avg_cpu_pct None (no divide-by-zero panic).
        let m = raw_to_build_metrics(Some(raw), 0, 4);
        assert_eq!(m.avg_cpu_pct, None);
        // cpu_count = 0 → avg_cpu_pct None.
        let m = raw_to_build_metrics(Some(raw), 1_000, 0);
        assert_eq!(m.avg_cpu_pct, None);
        assert_eq!(m.peak_ram_mb, Some(2));
        assert_eq!(m.cpu_time_ms, Some(1_000));
        assert_eq!(m.disk_read_bytes, Some(10));
        assert_eq!(m.disk_write_bytes, Some(20));
    }

    #[test]
    fn raw_to_metrics_computes_avg_cpu_pct() {
        let raw = BuildMetricsRaw {
            peak_ram_bytes: None,
            cpu_usage_usec: Some(8_000_000), // 8000 ms of CPU
            disk_read_bytes: 0,
            disk_write_bytes: 0,
            oom_killed: false,
        };
        // 8000 cpu-ms over 4000 ms wall on 4 cores = 8000/(4000*4)*100 = 50%.
        let m = raw_to_build_metrics(Some(raw), 4_000, 4);
        assert_eq!(m.cpu_time_ms, Some(8_000));
        assert_eq!(m.avg_cpu_pct, Some(50.0));
    }

    #[tokio::test]
    async fn load_products_returns_empty_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let products = load_products(dir.path().to_str().unwrap()).await;
        assert!(products.is_empty());
    }

    #[tokio::test]
    async fn load_products_parses_hydra_lines() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("nix-support");
        tokio::fs::create_dir_all(&support).await.unwrap();
        let report_path = dir.path().join("index.html");
        tokio::fs::write(&report_path, b"<html></html>")
            .await
            .unwrap();

        let products_file = support.join("hydra-build-products");
        let line = format!("file html {}", report_path.display());
        tokio::fs::write(&products_file, line).await.unwrap();

        let products = load_products(dir.path().to_str().unwrap()).await;
        assert_eq!(products.len(), 1);
        assert_eq!(products[0].file_type, "file");
        assert_eq!(products[0].subtype, "html");
        assert_eq!(products[0].name, "index.html");
        assert_eq!(products[0].size, Some(13));
    }
}

#[cfg(test)]
mod classify_tests {
    use super::{classify_build_error, looks_like_oom};
    use proto::messages::BuildFailureKind;

    #[test]
    fn builder_nonzero_is_permanent() {
        assert_eq!(
            classify_build_error("build failed: builder for '/nix/store/x.drv' failed with exit code 1"),
            BuildFailureKind::Permanent
        );
    }

    #[test]
    fn oom_signature_is_transient() {
        assert_eq!(
            classify_build_error("build failed: gcc: fatal error: Killed signal terminated; out of memory"),
            BuildFailureKind::Transient
        );
        assert!(looks_like_oom("cc1plus: out of memory allocating 1048576 bytes"));
        assert!(looks_like_oom("Killed"));
        assert!(looks_like_oom("oom-killer: invoked"));
        assert!(!looks_like_oom("error: undefined reference to `foo'"));
    }
}
