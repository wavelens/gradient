/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use futures::StreamExt as _;
use gradient_derivation::parse_drv;
use gradient_sources::get_hash_from_path;
use gradient_util::hydra::parse_hydra_product_line;
use gradient_util::store_path::{nix_store_path, strip_store_prefix};
use gradient_wire::messages::{BuildOutput, BuildProduct, BuildSpec};
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

use super::build_metrics::{BuildHost, RUNNING_BUILDS, ResourceUsage, build_metrics};
use super::derivation::get_basic_derivation;
pub use super::failure::BuildError;
use super::failure::classify_build_error;

pub(super) struct ParsedDerivation {
    drv: gradient_derivation::Derivation,
    harmonia_path: StorePath,
    basic_drv: BasicDerivation,
}

impl ParsedDerivation {
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

    #[allow(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub(super) async fn realize(
        &self,
        store: &LocalNixStore,
        task_index: u32,
        updater: &mut JobUpdater,
        drv_path: &str,
        max_silent_secs: Option<u64>,
        abort: &mut watch::Receiver<bool>,
        log_limits: crate::executor::log_limit::LogRateLimits,
        build_cores: u32,
        mode: BuildMode,
    ) -> Result<BuildResult, BuildError> {
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
                BuildError::transient(anyhow::anyhow!(
                    "set_options failed for {}: {}",
                    drv_path,
                    e
                ))
            })?;

        // The connection is marked broken on abort or timeout. The nix-daemon is killing the
        // in-flight build only once its socket closes.
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
                let logs = client.build_derivation(harmonia_path, basic_drv, mode);
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

        match drained {
            Drained::Completed(result) => Ok(result),
            Drained::Aborted => {
                guard.mark_broken();
                Err(BuildError::aborted(drv_path))
            }
            Drained::Timeout(e) => {
                guard.mark_broken();
                Err(BuildError::timeout(e))
            }
        }
    }

    pub(super) async fn outputs(
        &self,
        result: BuildResultInner,
        updater: &mut JobUpdater,
        task_index: u32,
        drv_path: &str,
        log_fetch_from_store: bool,
    ) -> Result<(Vec<BuildOutput>, bool), BuildError> {
        match result {
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
                Ok((outputs, substituted))
            }

            BuildResultInner::Failure(f) => {
                let msg = String::from_utf8_lossy(&f.error_msg).to_string();
                warn!(drv = %drv_path, error = %msg, "build failed");
                let kind = classify_build_error(&msg);
                Err(BuildError::new(
                    kind,
                    anyhow::anyhow!("{}", gradient_sources::strip_nix_log_tail(&msg)),
                ))
            }
        }
    }
}

/// The daemon's `built_outputs` is authoritative for CA and deferred outputs. An empty map is
/// falling back to the `.drv` output paths, which is correct for input-addressed drvs and FODs.
/// Old protocols and already-valid FOD outputs are both producing an empty map. Empty CA paths
/// are skipped, and the caller is failing the build when no pairs survive.
fn output_pairs_from_built_or_drv(
    built_outputs: &BTreeMap<
        harmonia_store_derivation::derived_path::OutputName,
        harmonia_store_derivation::realisation::UnkeyedRealisation,
    >,
    drv: &gradient_derivation::Derivation,
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

async fn build_mode(store: &LocalNixStore, task: &BuildSpec) -> anyhow::Result<BuildMode> {
    for output in task.outputs.iter().filter(|o| !o.path.is_empty()) {
        if store.is_hidden(&output.path).await? {
            return Ok(BuildMode::Repair);
        }
    }
    Ok(BuildMode::Normal)
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub async fn build_derivation(
    store: &LocalNixStore,
    task: &BuildSpec,
    task_index: u32,
    updater: &mut JobUpdater,
    abort: &mut watch::Receiver<bool>,
    log_limits: crate::executor::log_limit::LogRateLimits,
    log_fetch_from_store: bool,
    host: BuildHost,
) -> Result<Vec<BuildOutput>, BuildError> {
    let parsed = ParsedDerivation::load(&task.drv_path)
        .await
        .map_err(BuildError::transient)?;
    let mode = build_mode(store, task)
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
        host.build_cores,
        mode,
    );

    let running = RUNNING_BUILDS.start();
    let started = std::time::Instant::now();
    let realized = match task.timeout_secs.map(std::time::Duration::from_secs) {
        Some(d) => tokio::time::timeout(d, realize).await.unwrap_or_else(|_| {
            Err(BuildError::timeout(anyhow::anyhow!(
                "build exceeded wall-clock timeout of {}s",
                d.as_secs()
            )))
        }),
        None => realize.await,
    };

    let usage = realized.as_ref().map(ResourceUsage::of).unwrap_or_default();
    let metrics = build_metrics(usage, started.elapsed().as_millis() as u64, host, &running);
    drop(running);
    let built = match realized {
        Ok(result) => {
            parsed
                .outputs(
                    result.inner,
                    updater,
                    task_index,
                    &task.drv_path,
                    log_fetch_from_store,
                )
                .await
        }
        Err(e) => Err(e),
    };

    let (outputs, substituted) = built.map_err(|e| e.with_metrics(metrics.clone()))?;
    let output_paths: Vec<String> = outputs.iter().map(|o| o.store_path.clone()).collect();
    store
        .reveal(&output_paths)
        .await
        .map_err(BuildError::transient)?;
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

pub(super) async fn forward_store_build_log(
    updater: &mut JobUpdater,
    task_index: u32,
    drv_path: &str,
) {
    let log_dir = crate::nix::log::nix_log_dir();
    match crate::nix::log::read_store_build_log(&log_dir, drv_path) {
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

#[derive(Default)]
struct LogStreamStats {
    total_msgs: u64,
    forwarded_lines: u64,
    forwarded_bytes: u64,
    send_failures: u64,
}

enum DrainOutcome {
    Completed(LogStreamStats),
    Aborted,
}

enum NextLog {
    Message(LogMessage),
    StreamEnd,
    Aborted,
}

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

async fn maybe_silent_timeout(silent: Option<std::time::Duration>) {
    match silent {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

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
        warn!(drv = %drv_path, "daemon emitted zero LogMessages during build");
    }
}

fn is_orchestration_activity(activity_type: ActivityType, text: &str) -> bool {
    match activity_type {
        ActivityType::Build | ActivityType::Builds => true,
        ActivityType::Unknown => text.starts_with("querying info about missing paths"),
        _ => false,
    }
}

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
        let msg = start_activity(
            ActivityType::Unknown,
            "copying '/nix/store/xxxx' to the store",
        );
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

    fn drv_with_outputs(outputs: Vec<(&str, &str)>) -> gradient_derivation::Derivation {
        gradient_derivation::Derivation {
            outputs: outputs
                .into_iter()
                .map(|(name, path)| gradient_derivation::DerivationOutput {
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

    #[tokio::test]
    async fn load_products_reports_a_missing_artefact_without_size() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("nix-support");
        tokio::fs::create_dir_all(&support).await.unwrap();
        let line = format!("file iso {}", dir.path().join("image.iso").display());
        tokio::fs::write(support.join("hydra-build-products"), line)
            .await
            .unwrap();

        let products = load_products(dir.path().to_str().unwrap()).await;
        assert_eq!(products.len(), 1);
        assert_eq!(products[0].name, "image.iso");
        assert_eq!(products[0].size, None);
    }
}
