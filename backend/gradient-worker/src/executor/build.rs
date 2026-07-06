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
use futures::StreamExt as _;
use gradient_db::parse_drv;
use gradient_exec::path_utils::{nix_store_path, strip_store_prefix};
use gradient_proto::messages::{BuildOutput, BuildProduct, BuildTask};
use gradient_sources::get_hash_from_path;
use gradient_util::hydra::parse_hydra_product_line;
use harmonia_protocol::daemon_wire::types2::{BuildMode, BuildResult, BuildResultInner};
use harmonia_protocol::log::{ActivityType, Field, LogMessage, ResultType, Verbosity};
use harmonia_protocol::types::ClientOptions;
use harmonia_store_derivation::derivation::BasicDerivation;
use harmonia_store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use std::collections::BTreeMap;
use std::pin::{Pin, pin};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::nix::store::LocalNixStore;
use crate::proto::job::JobUpdater;

use super::build_metrics::{
    CgroupSampler, NetworkPeakSampler, assemble_build_metrics, daemon_cpu_usec,
};
use super::derivation::get_basic_derivation;
pub use super::failure::BuildError;
use super::failure::classify_build_error;

// ── Type-state pipeline ───────────────────────────────────────────────────────

/// A `.drv` file read from disk and parsed into all data needed to call
/// `DaemonStore::build_derivation`.
///
/// Obtain via [`ParsedDerivation::load`]; advance to built outputs via
/// [`ParsedDerivation::realize`].
pub(super) struct ParsedDerivation {
    drv: gradient_db::Derivation,
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

        let basic_drv = get_basic_derivation(&path, &drv).await?;

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
    /// running.  Returns one [`BuildOutput`] per output name (`nar_size` and
    /// `nar_hash` are `None` at this stage, filled in by the compress step)
    /// plus a `substituted` flag - true when the daemon reported the outputs as
    /// already valid (empty `built_outputs`), i.e. no work was performed.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn realize(
        self,
        store: &LocalNixStore,
        task_index: u32,
        updater: &mut JobUpdater,
        drv_path: &str,
        max_silent_secs: Option<u64>,
        abort: &mut watch::Receiver<bool>,
        log_limits: crate::executor::log_limit::LogRateLimits,
        log_fetch_from_store: bool,
        build_cores: u32,
    ) -> Result<(Vec<BuildOutput>, bool, Option<u64>), BuildError> {
        let mut guard = store.acquire().await.map_err(BuildError::transient)?;

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
        opts.build_cores = build_cores;

        guard
            .execute(|client| async move { client.set_options(&opts).await })
            .await
            .map_err(|e| {
                BuildError::transient(anyhow::anyhow!("set_options failed for {}: {}", drv_path, e))
            })?;

        // `build_derivation` is a `Stream<Item=LogMessage> + Future<BuildResult>`,
        // drained inside `execute` so a protocol error discards the connection.
        // On abort or timeout we `mark_broken` explicitly: closing the socket
        // makes the nix-daemon kill the in-flight build.
        let silent = max_silent_secs.map(std::time::Duration::from_secs);
        enum Drained {
            Completed(BuildResult),
            Aborted,
            Timeout(anyhow::Error),
        }
        let harmonia_path = &self.harmonia_path;
        let basic_drv = &self.basic_drv;
        let updater_ref = &mut *updater;
        let abort_ref = &mut *abort;
        let drained = guard
            .execute(|client| async move {
                let logs = client.build_derivation(harmonia_path, basic_drv, BuildMode::Normal);
                let mut logs = pin!(logs);
                match drain_build_logs_with_timeout(
                    logs.as_mut(),
                    updater_ref,
                    task_index,
                    silent,
                    abort_ref,
                    log_limits,
                )
                .await
                {
                    Ok(DrainOutcome::Completed(stats)) => {
                        log_stream_summary(&stats, drv_path);
                        Ok(Drained::Completed(logs.await?))
                    }
                    Ok(DrainOutcome::Aborted) => Ok(Drained::Aborted),
                    Err(e) => Ok(Drained::Timeout(e)),
                }
            })
            .await
            .map_err(|e| {
                BuildError::transient(anyhow::anyhow!(
                    "build_derivation failed for {}: {}",
                    drv_path,
                    e
                ))
            })?;

        let result = match drained {
            Drained::Completed(r) => r,
            Drained::Aborted => {
                guard.mark_broken();
                return Err(BuildError::aborted(drv_path));
            }
            Drained::Timeout(e) => {
                guard.mark_broken();
                return Err(BuildError::timeout(e));
            }
        };

        // CPU time the daemon read from the build cgroup before destroying it.
        let cpu_usec = daemon_cpu_usec(result.cpu_user, result.cpu_system);

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
                let substituted = s.built_outputs.is_empty();
                if substituted {
                    debug!(
                        drv = %drv_path,
                        recovered = pairs.len(),
                        "daemon returned empty built_outputs (output already valid or legacy \
                         protocol); recovering input-addressed/FOD paths from .drv"
                    );
                    if log_fetch_from_store {
                        forward_store_build_log(updater, task_index, drv_path).await;
                    }
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
                Ok((outputs, substituted, cpu_usec))
            }

            BuildResultInner::Failure(f) => {
                let msg = String::from_utf8_lossy(&f.error_msg).to_string();
                warn!(drv = %drv_path, error = %msg, "build failed");
                let kind = classify_build_error(&msg);
                Err(BuildError::new(
                    kind,
                    anyhow::anyhow!("build failed: {}", msg),
                ))
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
    drv: &gradient_db::Derivation,
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
    cgroup_state_dir: &str,
    log_limits: crate::executor::log_limit::LogRateLimits,
    log_fetch_from_store: bool,
    build_cores: u32,
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
        log_limits,
        log_fetch_from_store,
        build_cores,
    );

    let net_sampler = build_metrics.then(NetworkPeakSampler::start);
    let cgroup_sampler = build_metrics
        .then(|| CgroupSampler::start(cgroup_state_dir.to_string(), cgroup_root.to_string()));
    let started = std::time::Instant::now();
    let realize_result: Result<(Vec<BuildOutput>, bool, Option<u64>), BuildError> =
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

    // Assemble metrics (wall-clock + sampled peak RAM/disk + daemon CPU) and
    // ship them inline with the `BuildOutput`. On failure they are dropped with
    // the error; the scheduler records metrics only for completed builds.
    let build_time_ms = started.elapsed().as_millis() as u64;
    let peak_network_mbps = match net_sampler {
        Some(s) => s.finish().await,
        None => None,
    };
    let cgroup_raw = match cgroup_sampler {
        Some(s) => s.finish().await,
        None => None,
    };
    let cpu_usec = realize_result.as_ref().ok().and_then(|(_, _, c)| *c);
    let metrics = assemble_build_metrics(cgroup_raw, cpu_usec, build_time_ms, peak_network_mbps);

    let (outputs, substituted, _) = realize_result?;
    updater
        .report_build_output(
            task.build_id.clone(),
            outputs.clone(),
            Some(metrics),
            substituted,
        )
        .await
        .map_err(BuildError::transient)?;
    Ok(outputs)
}

// ── Log helpers ───────────────────────────────────────────────────────────────

/// When a derivation is already built locally the daemon produces no log.
/// Read nix's stored `.bz2` log and forward it so the UI still shows output.
/// Best-effort: missing logs and read errors are logged at debug and ignored.
async fn forward_store_build_log(updater: &mut JobUpdater, task_index: u32, drv_path: &str) {
    match crate::nix::log::read_store_build_log(drv_path) {
        Ok(Some(text)) if !text.is_empty() => {
            const SEND_CHUNK: usize = 256 * 1024;
            let bytes = text.into_bytes();
            for slice in bytes.chunks(SEND_CHUNK) {
                if let Err(e) = updater.send_log_chunk(task_index, slice.to_vec()).await {
                    warn!(error = %e, "failed to forward stored build log; continuing");
                    break;
                }
            }
            debug!(drv = %drv_path, "forwarded stored nix build log for already-built derivation");
        }
        Ok(_) => debug!(drv = %drv_path, "no stored nix build log for already-built derivation"),
        Err(e) => debug!(drv = %drv_path, error = %e, "failed to read stored nix build log"),
    }
}

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
    log_limits: crate::executor::log_limit::LogRateLimits,
) -> Result<DrainOutcome>
where
    S: futures::Stream<Item = LogMessage>,
{
    use crate::executor::log_limit::LogRateLimiter;
    let mut stats = LogStreamStats::default();
    let mut limiter = LogRateLimiter::from_limits(log_limits);
    let started = std::time::Instant::now();
    let mut limit_hit = false;
    loop {
        let msg = match next_log_event(logs.as_mut(), silent, abort).await? {
            NextLog::Message(msg) => msg,
            NextLog::StreamEnd => break,
            NextLog::Aborted => return Ok(DrainOutcome::Aborted),
        };
        stats.total_msgs += 1;
        if let Some(line) = log_message_to_text(&msg) {
            if limit_hit {
                continue;
            }
            let len = line.len();
            if !limiter.admit(len as u64, started.elapsed().as_secs_f64()) {
                limit_hit = true;
                let _ = updater
                    .send_log_chunk(
                        task_index,
                        b"\x1b[0m[gradient: log truncated \xe2\x80\x94 rate limit exceeded]\n"
                            .to_vec(),
                    )
                    .await;
                warn!("build log rate limit exceeded; truncating remaining output");
                continue;
            }
            // Log streaming is best-effort - never fail the build because the
            // server connection hiccupped.
            match updater.send_log_chunk(task_index, line.into_bytes()).await {
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

/// Nix build-orchestration activities that are not the builder's own output:
/// the "building '/nix/store/…drv'" announcement and the "querying info about
/// missing paths" summary, both emitted on every build. `Unknown` is overloaded
/// (it also carries useful "copying '…' to the store" lines for in-daemon
/// builtins), so only its fixed missing-paths summary is dropped.
fn is_orchestration_activity(activity_type: ActivityType, text: &str) -> bool {
    match activity_type {
        ActivityType::Build | ActivityType::Builds => true,
        ActivityType::Unknown => text.starts_with("querying info about missing paths"),
        _ => false,
    }
}

/// Extract a forwardable log line from a harmonia daemon log message.
///
/// Captures:
/// - `Message`: high-level messages (errors, warnings, status notes).
/// - `StartActivity`: activity descriptions ("copying '/nix/store/…'" etc.),
///   minus the build orchestration filtered by [`is_orchestration_activity`].
///   Kept for builtins (fetchurl, path) that run inside the daemon rather than
///   in a sandbox and therefore never produce `BuildLogLine` results.
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
            if s.is_empty() || is_orchestration_activity(a.activity_type, &s) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use harmonia_protocol::log::{Activity, ActivityResult, Message};

    fn start_activity(activity_type: ActivityType, text: &'static str) -> LogMessage {
        LogMessage::StartActivity(Activity {
            fields: vec![],
            id: 1,
            level: Verbosity::Info,
            parent: 0,
            text: text.into(),
            activity_type,
        })
    }

    #[test]
    fn drops_build_announcement_activity() {
        let msg = start_activity(ActivityType::Build, "building '/nix/store/xxxx-foo.drv'");
        assert_eq!(log_message_to_text(&msg), None);
    }

    #[test]
    fn drops_missing_paths_query_summary() {
        let msg = start_activity(ActivityType::Unknown, "querying info about missing paths");
        assert_eq!(log_message_to_text(&msg), None);
    }

    #[test]
    fn keeps_copy_to_store_activity() {
        let msg = start_activity(ActivityType::Unknown, "copying '/nix/store/xxxx' to the store");
        assert_eq!(
            log_message_to_text(&msg).as_deref(),
            Some("copying '/nix/store/xxxx' to the store\n")
        );
    }

    #[test]
    fn keeps_builder_output_line() {
        let msg = LogMessage::Result(ActivityResult {
            fields: vec![Field::String("hello from builder".into())],
            id: 1,
            result_type: ResultType::BuildLogLine,
        });
        assert_eq!(
            log_message_to_text(&msg).as_deref(),
            Some("hello from builder\n")
        );
    }

    #[test]
    fn keeps_error_messages() {
        let msg = LogMessage::Message(Message {
            level: Verbosity::Error,
            text: "error: build failed".into(),
        });
        assert_eq!(
            log_message_to_text(&msg).as_deref(),
            Some("error: build failed\n")
        );
    }

    fn drv_with_outputs(outputs: Vec<(&str, &str)>) -> gradient_db::Derivation {
        gradient_db::Derivation {
            outputs: outputs
                .into_iter()
                .map(|(name, path)| gradient_db::DerivationOutput {
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
