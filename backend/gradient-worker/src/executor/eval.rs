/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use super::progress_report::{ChangeReporter, thunk_progress};
use crate::worker_pool::{WorkerPoolResolver, budgeted_pool_size};
use anyhow::{Context, Result};
use futures::stream::{FuturesOrdered, FuturesUnordered, StreamExt as _};
use gradient_derivation::parse_drv;
use gradient_sources::{AttrError, DerivationResolver, FlakeDiscovery};
use gradient_wire::messages::{
    DiscoveredDerivation, EvalAttrCost, EvalStatsReport, FlakeJob, FlakeOutputNode, FlakeSource,
};
use gradient_wire::types::{EvalMessageLevel, attr_eval_source};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

fn abort_err() -> anyhow::Error {
    crate::executor::failure::JobAborted("evaluation aborted by server".to_owned()).into()
}

async fn unless_aborted<T>(
    abort: &mut super::AbortSignal,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        () = abort.aborted() => Err(abort_err()),
        out = work => out,
    }
}

#[derive(Debug)]
pub struct CorruptEvalCache {
    pub fingerprint: String,
}

impl std::fmt::Display for CorruptEvalCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupt eval-cache blob {}", self.fingerprint)
    }
}
impl std::error::Error for CorruptEvalCache {}

fn is_corrupt_eval_cache_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("database disk image is malformed")
        || m.contains("file is not a database")
        || m.contains("no such table")
}

fn eval_cache_fingerprint_from_error(msg: &str) -> Option<String> {
    const MARKER: &str = "eval-cache-v6/";
    let rest = &msg[msg.find(MARKER)? + MARKER.len()..];
    let fp = &rest[..rest.find(".sqlite")?];
    (!fp.is_empty() && fp.bytes().all(|b| b.is_ascii_hexdigit())).then(|| fp.to_string())
}

fn corrupt_eval_cache(msg: &str) -> Option<CorruptEvalCache> {
    if !is_corrupt_eval_cache_error(msg) {
        return None;
    }
    eval_cache_fingerprint_from_error(msg).map(|fingerprint| CorruptEvalCache { fingerprint })
}

fn is_shareable_eval_cache(bytes: &[u8]) -> bool {
    const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

    if !bytes.starts_with(SQLITE_MAGIC) {
        warn!(
            len = bytes.len(),
            "eval-cache blob is not a SQLite database; not pushing"
        );
        return false;
    }

    let has_schema = bytes
        .windows(b"CREATE TABLE".len())
        .any(|w| w.eq_ignore_ascii_case(b"CREATE TABLE"));
    if !has_schema {
        warn!(
            len = bytes.len(),
            "eval-cache blob carries no schema; not pushing"
        );
    }

    has_schema
}

async fn delete_eval_cache_blob(sqlite_path: &str) {
    for suffix in ["", "-wal", "-shm"] {
        let p = format!("{sqlite_path}{suffix}");
        match tokio::fs::remove_file(&p).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(path = %p, error = %e, "failed to delete corrupt eval-cache blob"),
        }
    }
}

fn explicit_attr_set(wildcards: &[String]) -> HashSet<String> {
    wildcards
        .iter()
        .filter_map(|w| {
            let (exclude, segs) = crate::nix::wildcard_walk::parse_pattern(w);
            (!exclude && !segs.iter().any(|s| s == "*" || s == "#")).then(|| segs.join("."))
        })
        .collect()
}

fn unmatched_target_errors(wildcards: &[String]) -> Vec<String> {
    let mut errors: Vec<String> = explicit_attr_set(wildcards)
        .into_iter()
        .map(|attr| format!("target '{attr}' matched no derivations in the flake"))
        .collect();
    errors.sort_unstable();
    errors
}

const DRV_READ_CONCURRENCY: usize = 256;

const EVAL_RAM_SHARE: f64 = 0.75;

const TOTAL_RAM_FALLBACK_BYTES: u64 = 4 * 1024 * 1024 * 1024;

fn total_memory_bytes() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let total = sys.total_memory();
    if total == 0 {
        TOTAL_RAM_FALLBACK_BYTES
    } else {
        total
    }
}

use crate::proto::job::JobUpdater;
use crate::traits::{DrvReader, FsDrvReader, JobReporter};

pub struct WorkerEvaluator {
    resolver: Arc<WorkerPoolResolver>,
    eval_cache_share: bool,
}

impl WorkerEvaluator {
    pub(crate) fn resolver(&self) -> &Arc<WorkerPoolResolver> {
        &self.resolver
    }

    pub fn new(
        fork_workers: usize,
        max_eval_rss: u64,
        min_free_ram_mb: u64,
        eval_cache_dir: String,
        eval_cache_share: bool,
    ) -> Self {
        let total_ram = total_memory_bytes();
        let ram_budget = (total_ram as f64 * EVAL_RAM_SHARE) as u64;
        let pool_size = budgeted_pool_size(fork_workers, max_eval_rss, ram_budget);
        if pool_size < fork_workers {
            info!(
                fork_workers,
                pool_size,
                max_eval_rss,
                ram_budget,
                "eval pool sized down to fit the memory budget"
            );
        }

        let min_free_bytes = crate::worker_pool::memory_guard_bytes(min_free_ram_mb, total_ram);
        let resolver = Arc::new(WorkerPoolResolver::new(
            pool_size,
            max_eval_rss,
            eval_cache_dir,
        ));
        resolver.start_memory_reaper(min_free_bytes);

        Self {
            resolver,
            eval_cache_share,
        }
    }

    pub async fn shutdown(&self) {
        self.resolver.shutdown().await;
    }
}

impl Clone for WorkerEvaluator {
    fn clone(&self) -> Self {
        Self {
            resolver: self.resolver.clone(),
            eval_cache_share: self.eval_cache_share,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn evaluate_flake(_job: &FlakeJob, updater: &mut JobUpdater) -> Result<()> {
    let updater: &JobUpdater = updater;
    updater.report_evaluating_flake().await
}

pub fn required_local_source(source: &FlakeSource) -> Option<&str> {
    match source {
        FlakeSource::Cached { store_path } => Some(store_path.as_str()),
        FlakeSource::Repository { .. } => None,
    }
}

const EVAL_BATCH_SIZE: usize = 50;

#[tracing::instrument(level = "debug", skip_all)]
pub async fn evaluate_derivations(
    evaluator: &WorkerEvaluator,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    updater: &mut JobUpdater,
    abort: &mut super::AbortSignal,
) -> Result<()> {
    let repo = build_flake_url(job, local_flake_path);
    let start = Instant::now();
    let eval_overrides = eval_input_overrides(job, local_flake_path);

    let fingerprint = if evaluator.eval_cache_share {
        match evaluator
            .resolver
            .fingerprint(repo.clone(), &eval_overrides)
            .await
        {
            Ok(fp) => fp,
            Err(e) => {
                warn!(error = %e, "eval-cache fingerprint failed; evaluating local-only");
                None
            }
        }
    } else {
        None
    };

    let cache_path = fingerprint.as_ref().map(|fp| {
        format!(
            "{}/eval-cache-v6/{fp}.sqlite",
            evaluator.resolver.eval_cache_dir()
        )
    });

    if let (Some(fp), Some(path)) = (fingerprint.as_ref(), cache_path.as_ref()) {
        match updater.pull_eval_cache(fp).await {
            Ok(Some(bytes)) => {
                if let Err(e) = write_eval_cache_blob(path, &bytes).await {
                    warn!(error = %e, %path, "failed to stage pulled eval-cache blob");
                }
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, "eval-cache pull failed; evaluating local-only"),
        }
    }

    let thunks = evaluator.resolver.live_thunks();
    thunks.reset();
    let sink = updater.eval_progress_sink();
    let snapshot = || thunk_progress(thunks.total());
    let mut reporter = ChangeReporter::default();
    let evaluated = tokio::select! {
        evaluated = evaluate_derivations_with(
            &*evaluator.resolver,
            &FsDrvReader,
            job,
            local_flake_path,
            updater,
            abort,
        ) => evaluated,
        never = reporter.run(&*sink, snapshot) => match never {},
    };
    reporter.flush(&*sink, &snapshot).await;
    let EvalOutcome {
        flake_nodes,
        cacheable,
    } = match evaluated {
        Ok(outcome) => outcome,
        Err(e) => {
            if let Some(corrupt) = e.downcast_ref::<CorruptEvalCache>() {
                let path = format!(
                    "{}/eval-cache-v6/{}.sqlite",
                    evaluator.resolver.eval_cache_dir(),
                    corrupt.fingerprint
                );
                warn!(fingerprint = %corrupt.fingerprint, "eval-cache corrupt; dropping local blob + recycling eval workers");
                delete_eval_cache_blob(&path).await;
                evaluator.resolver.release_evaluators().await;
            }
            return Err(e);
        }
    };

    let totals = evaluator.resolver.take_eval_stats();
    if totals.total_thunks > 0 || !totals.per_entry_point.is_empty() {
        let report =
            build_eval_stats_report(totals, flake_nodes, start.elapsed().as_millis() as u64);
        if let Err(e) = updater.report_eval_stats(report).await {
            warn!(error = %e, "failed to send eval stats report");
        }
    }

    // An eval that discovered nothing is leaving an empty cache. Sharing it would poison
    // every later eval of the same flake with a schemaless blob.
    if !cacheable {
        debug!("evaluation produced no derivations; keeping its eval-cache local");
        return Ok(());
    }

    if cache_path.is_some()
        && let Err(e) = evaluator
            .resolver
            .checkpoint_cache(repo.clone(), &eval_overrides)
            .await
    {
        warn!(error = %e, "eval-cache checkpoint failed; pushing as-is");
    }

    if let (Some(fp), Some(path)) = (fingerprint.as_ref(), cache_path.as_ref())
        && let Ok(bytes) = tokio::fs::read(path).await
        && is_shareable_eval_cache(&bytes)
        && let Err(e) = updater.push_eval_cache(fp, bytes).await
    {
        warn!(error = %e, "eval-cache push failed; continuing");
    }

    Ok(())
}

/// A previous evaluation of the same flake is leaving `-wal`/`-shm` sidecars next to the blob.
/// SQLite is reading them as part of whatever main file it finds. Staging over them would mix
/// server pages with pages from the last local eval. The sidecars are removed before the bytes
/// land.
async fn write_eval_cache_blob(path: &str, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    for suffix in ["-wal", "-shm"] {
        let sidecar = format!("{path}{suffix}");
        match tokio::fs::remove_file(&sidecar).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(path = %sidecar, error = %e, "failed to drop stale eval-cache sidecar"),
        }
    }

    tokio::fs::write(path, bytes).await?;
    Ok(())
}

fn build_flake_url(job: &FlakeJob, local_flake_path: Option<&str>) -> String {
    if let Some(path) = local_flake_path {
        return format!("path:{}", path);
    }
    match &job.source {
        FlakeSource::Repository { url, commit } => gradient_types::NixFlakeUrl::new(url, commit)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| url.clone()),
        FlakeSource::Cached { store_path } => format!("path:{}", store_path),
    }
}

fn eval_input_overrides(job: &FlakeJob, local_flake_path: Option<&str>) -> Vec<(String, String)> {
    let declared: std::collections::BTreeSet<String> = local_flake_path
        .and_then(|p| std::fs::read(std::path::Path::new(p).join("flake.lock")).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|lock| {
            let root = lock.get("root").and_then(|v| v.as_str()).unwrap_or("root");
            lock.get("nodes")
                .and_then(|n| n.get(root))
                .and_then(|r| r.get("inputs"))
                .and_then(|i| i.as_object())
                .map(|o| o.keys().cloned().collect())
        })
        .unwrap_or_default();

    if declared.is_empty() {
        return job
            .input_overrides
            .iter()
            .filter(|o| !gradient_util::glob::is_pattern(&o.input_name))
            .filter_map(|o| o.url.clone().map(|u| (o.input_name.clone(), u)))
            .collect();
    }

    let raw: Vec<(String, Option<String>)> = job
        .input_overrides
        .iter()
        .map(|o| (o.input_name.clone(), o.url.clone()))
        .collect();
    let (resolved, _warnings) = gradient_util::glob::expand_overrides(&raw, &declared);
    resolved
        .into_iter()
        .filter_map(|(n, url)| url.map(|u| (n, u)))
        .collect()
}

#[tracing::instrument(level = "debug", skip_all, fields(size = wave.len()))]
async fn parse_drv_wave(
    drv_reader: &dyn DrvReader,
    wave: &[(Option<String>, String)],
) -> Result<Vec<(gradient_derivation::Derivation, u64)>> {
    let mut futs: FuturesUnordered<_> = wave
        .iter()
        .enumerate()
        .map(|(i, (_, drv_path))| {
            let drv_path = drv_path.clone();
            async move {
                let bytes = drv_reader.read_drv(&drv_path).await.with_context(|| {
                    format!(
                        "cannot read .drv {drv_path} during closure walk; aborting eval \
                         to avoid silently dropping dependencies"
                    )
                })?;
                let parsed = parse_drv(&bytes).with_context(|| {
                    format!(
                        "cannot parse .drv {drv_path} during closure walk; aborting eval \
                         to avoid silently dropping dependencies"
                    )
                })?;
                Ok::<_, anyhow::Error>((i, (parsed, drv_nar_size(bytes.len()))))
            }
        })
        .collect();

    let mut slots: Vec<Option<(gradient_derivation::Derivation, u64)>> =
        (0..wave.len()).map(|_| None).collect();
    while let Some(result) = futs.next().await {
        let (i, drv) = result?;
        slots[i] = Some(drv);
    }

    Ok(slots
        .into_iter()
        .map(|s| s.expect("every wave slot was filled"))
        .collect())
}

const PUBLISH_BACKLOG: usize = 64;

struct Flush {
    paths: Vec<(String, Option<u64>)>,
    derivations: Vec<DiscoveredDerivation>,
    warnings: Vec<String>,
}

async fn publish(reporter: &dyn JobReporter, mut flushes: mpsc::Receiver<Flush>) -> Result<()> {
    let mut pushed = HashSet::new();
    let mut pushes = FuturesOrdered::new();
    loop {
        tokio::select! {
            biased;
            Some(cached) = pushes.next(), if !pushes.is_empty() => report(reporter, cached?).await?,
            flush = flushes.recv() => match flush {
                Some(mut flush) => {
                    let fresh = fresh_paths(&mut pushed, &mut flush);
                    pushes.push_back(async move {
                        reporter.push_paths(&fresh).await?;
                        Ok::<_, anyhow::Error>(flush)
                    });
                }
                None => break,
            },
        }
    }
    while let Some(cached) = pushes.next().await {
        report(reporter, cached?).await?;
    }

    Ok(())
}

fn fresh_paths(pushed: &mut HashSet<String>, flush: &mut Flush) -> Vec<(String, Option<u64>)> {
    std::mem::take(&mut flush.paths)
        .into_iter()
        .filter(|(path, _)| pushed.insert(path.clone()))
        .collect()
}

async fn report(reporter: &dyn JobReporter, flush: Flush) -> Result<()> {
    reporter
        .report_eval_result(flush.derivations, flush.warnings, vec![])
        .await
}

fn drv_nar_size(len: usize) -> u64 {
    (112 + len.div_ceil(8) * 8) as u64
}

struct ClosureWalker<'a> {
    drv_reader: &'a dyn DrvReader,
    batch: Vec<DiscoveredDerivation>,
    visited: HashSet<String>,
    queue: VecDeque<(Option<String>, String)>,
    walked: usize,
    start: Instant,
    paths: Vec<(String, Option<u64>)>,
    flushes: mpsc::Sender<Flush>,
}

impl<'a> ClosureWalker<'a> {
    fn new(
        drv_reader: &'a dyn DrvReader,
        root_drvs: &[(String, String)],
        flushes: mpsc::Sender<Flush>,
    ) -> Self {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        for (attr, drv) in root_drvs {
            if visited.insert(drv.clone()) {
                queue.push_back((Some(attr.clone()), drv.clone()));
            }
        }
        info!(roots = root_drvs.len(), "starting closure walk");
        Self {
            drv_reader,
            batch: Vec::new(),
            visited,
            queue,
            walked: 0,
            start: Instant::now(),
            paths: Vec::new(),
            flushes,
        }
    }

    async fn walk(
        mut self,
        reporter: &dyn JobReporter,
        abort: &mut super::AbortSignal,
        warnings: Vec<String>,
    ) -> Result<()> {
        while !self.queue.is_empty() {
            if abort.is_aborted() {
                return Err(abort_err());
            }
            self.process_wave(reporter).await?;
        }

        info!(
            walked = self.walked,
            elapsed_secs = self.start.elapsed().as_secs(),
            "closure walk complete"
        );

        self.flush(warnings).await
    }

    async fn flush(&mut self, warnings: Vec<String>) -> Result<()> {
        debug!(
            count = self.batch.len(),
            remaining = self.queue.len(),
            "flushing eval batch"
        );
        let flush = Flush {
            paths: std::mem::take(&mut self.paths),
            derivations: std::mem::take(&mut self.batch),
            warnings,
        };
        self.flushes
            .send(flush)
            .await
            .map_err(|_| anyhow::anyhow!("the eval publisher stopped before the walk"))
    }

    #[tracing::instrument(name = "wave", level = "debug", skip_all, fields(queued = self.queue.len()))]
    async fn process_wave(&mut self, reporter: &dyn JobReporter) -> Result<()> {
        let wave_size = self.queue.len().min(DRV_READ_CONCURRENCY);
        let wave: Vec<_> = (0..wave_size)
            .map(|_| self.queue.pop_front().expect("wave_size <= queue.len()"))
            .collect();

        let parsed_drvs = parse_drv_wave(self.drv_reader, &wave).await?;

        let mut new_deps: Vec<String> = Vec::new();
        for (drv, _) in &parsed_drvs {
            for (input_drv, _) in &drv.input_derivations {
                if !self.visited.contains(input_drv.as_str()) {
                    new_deps.push(input_drv.clone());
                }
            }
        }
        new_deps.sort_unstable();
        new_deps.dedup();

        for dep in &new_deps {
            self.visited.insert(dep.clone());
        }

        let known_set: HashSet<String> = if new_deps.is_empty() {
            HashSet::new()
        } else {
            reporter
                .query_known_derivations(new_deps.clone())
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "query_known_derivations failed; treating all as unknown");
                    vec![]
                })
                .into_iter()
                .collect()
        };

        if !known_set.is_empty() {
            debug!(pruned = known_set.len(), "BFS: pruning known subtrees");
        }

        for dep in new_deps {
            if !known_set.contains(&dep) {
                self.queue.push_back((None, dep));
            }
        }

        for ((attr, drv_path), (drv, nar_size)) in wave.into_iter().zip(parsed_drvs) {
            self.paths.push((drv_path.clone(), Some(nar_size)));
            self.paths
                .extend(drv.input_sources.iter().map(|src| (src.clone(), None)));
            self.batch.push(gradient_derivation::discovered_derivation(
                attr, drv_path, &drv,
            ));
            self.walked += 1;

            if self.walked.is_multiple_of(500) {
                info!(
                    walked = self.walked,
                    queued = self.queue.len(),
                    elapsed_secs = self.start.elapsed().as_secs(),
                    "closure walk progress"
                );
            }

            if self.batch.len() >= EVAL_BATCH_SIZE {
                self.flush(vec![]).await?;
            }
        }

        Ok(())
    }
}

async fn report_failed_attrs(updater: &mut dyn JobReporter, failed: &[AttrError]) -> Result<()> {
    for AttrError { attr, message } in failed {
        updater
            .send_eval_message(
                EvalMessageLevel::Error,
                &attr_eval_source(attr),
                &format!("failed to evaluate '{attr}': {message}"),
            )
            .await?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct EvalOutcome {
    pub flake_nodes: Vec<FlakeOutputNode>,
    pub cacheable: bool,
}

fn flake_kind(attr: &str) -> &'static str {
    match attr.split('.').next().unwrap_or("") {
        "packages" | "legacyPackages" => "package",
        "devShells" | "devShell" => "devShell",
        "checks" => "check",
        "apps" => "app",
        "nixosConfigurations" => "nixosConfiguration",
        _ => "other",
    }
}

fn flake_nodes_from_roots(root_drvs: &[(String, String)]) -> Vec<FlakeOutputNode> {
    root_drvs
        .iter()
        .map(|(attr, drv)| {
            let (parent, name) = match attr.rsplit_once('.') {
                Some((p, n)) => (Some(p.to_string()), n.to_string()),
                None => (None, attr.clone()),
            };

            FlakeOutputNode {
                path: attr.clone(),
                parent,
                name,
                kind: flake_kind(attr).to_string(),
                is_derivation: true,
                drv_path: Some(drv.clone()),
            }
        })
        .collect()
}

fn build_eval_stats_report(
    totals: crate::worker_pool::eval_stats::EvalStatsTotals,
    flake_nodes: Vec<FlakeOutputNode>,
    total_eval_ms: u64,
) -> EvalStatsReport {
    const MB: u64 = 1024 * 1024;
    EvalStatsReport {
        total_thunks: totals.total_thunks,
        fn_calls: totals.fn_calls,
        primop_calls: totals.primop_calls,
        lookups: totals.lookups,
        alloc_bytes: totals.alloc_bytes,
        peak_heap_mb: totals.peak_heap_bytes / MB,
        peak_rss_mb: totals.peak_rss_bytes / MB,
        total_eval_ms,
        per_entry_point: totals
            .per_entry_point
            .into_iter()
            .map(|c| EvalAttrCost {
                attr: c.attr,
                thunks: c.thunks,
                fn_calls: c.fn_calls,
                eval_ms: 0,
                alloc_bytes: c.alloc_bytes,
            })
            .collect(),
        flake_nodes,
        ..Default::default()
    }
}

pub async fn evaluate_derivations_with(
    resolver: &dyn DerivationResolver,
    drv_reader: &dyn DrvReader,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    updater: &mut dyn JobReporter,
    abort: &mut super::AbortSignal,
) -> Result<EvalOutcome> {
    if abort.is_aborted() {
        return Err(abort_err());
    }
    updater.report_evaluating_derivations().await?;

    let repo = build_flake_url(job, local_flake_path);
    let eval_overrides = eval_input_overrides(job, local_flake_path);

    debug!(repo = %repo, "listing flake derivations");
    let FlakeDiscovery {
        derivations,
        mut warnings,
        errors: mut failed,
    } = match unless_aborted(
        abort,
        resolver.list_flake_derivations(
            repo.clone(),
            job.wildcards.clone(),
            &eval_overrides,
            &gradient_sources::RefuseImports("the evaluation"),
        ),
    )
    .await
    {
        Err(e) if e.is::<crate::executor::failure::JobAborted>() => return Err(e),
        Ok(v) => v,
        Err(e) => {
            let err_msg = format!("list_flake_derivations failed: {:#}", e);
            warn!(error = %err_msg, "reporting eval error to server");
            let _ = updater
                .report_eval_result(vec![], vec![], vec![err_msg])
                .await;
            return Err(e).context("list_flake_derivations failed");
        }
    };

    if let Some(corrupt) = failed.iter().find_map(|e| corrupt_eval_cache(&e.message)) {
        warn!(fingerprint = %corrupt.fingerprint, "eval-cache corrupt during discovery; failing for self-heal");
        return Err(anyhow::Error::new(corrupt));
    }

    if derivations.is_empty() {
        warn!("no derivations found for evaluation");
        report_failed_attrs(updater, &failed).await?;
        updater
            .report_eval_result(vec![], warnings, unmatched_target_errors(&job.wildcards))
            .await?;
        return Ok(EvalOutcome {
            flake_nodes: Vec::new(),
            cacheable: false,
        });
    }

    let mut root_drvs: Vec<(String, String)> = Vec::new();
    for (attr, result) in derivations {
        match result {
            Ok((drv_path, _refs)) => root_drvs.push((attr, drv_path)),
            Err(e) => failed.push(AttrError {
                attr,
                message: format!("{e:#}"),
            }),
        }
    }

    if let Some(corrupt) = failed.iter().find_map(|e| corrupt_eval_cache(&e.message)) {
        warn!(fingerprint = %corrupt.fingerprint, "eval-cache corrupt during resolve; failing for self-heal");
        return Err(anyhow::Error::new(corrupt));
    }

    report_failed_attrs(updater, &failed).await?;

    if root_drvs.is_empty() {
        warn!("all attr resolutions failed");
        updater.report_eval_result(vec![], warnings, vec![]).await?;
        return Ok(EvalOutcome {
            flake_nodes: Vec::new(),
            cacheable: false,
        });
    }

    let flake_nodes = flake_nodes_from_roots(&root_drvs);
    resolver.release_evaluators().await;

    warnings.sort_unstable();
    warnings.dedup();

    let (flushes, published) = mpsc::channel(PUBLISH_BACKLOG);
    let walker = ClosureWalker::new(drv_reader, &root_drvs, flushes);
    let reporter: &dyn JobReporter = updater;
    tokio::try_join!(
        walker.walk(reporter, abort, warnings),
        publish(reporter, published),
    )?;
    Ok(EvalOutcome {
        flake_nodes,
        cacheable: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::fakes::derivation_resolver::FakeDerivationResolver;
    use gradient_test_support::prelude::*;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test-store")
    }

    #[test]
    fn corrupt_eval_cache_detects_and_extracts_fingerprint() {
        let fp = "ad2ae6ba345f9dce45b15a42b43f4dcb706dec975e7ea558779f3861f9f0b586";
        let msg = format!(
            "failed to evaluate 'checks': [nix::SQLiteError] ...: database disk image is malformed \
             (in '\u{1b}[35;1m/var/lib/gradient-worker/eval-cache/eval-cache-v6/{fp}.sqlite\u{1b}[0m')"
        );
        assert!(is_corrupt_eval_cache_error(&msg));
        assert_eq!(eval_cache_fingerprint_from_error(&msg).as_deref(), Some(fp));
        assert_eq!(corrupt_eval_cache(&msg).unwrap().fingerprint, fp);

        assert!(!is_corrupt_eval_cache_error(
            "failed to evaluate 'x': attribute missing"
        ));
        assert!(
            corrupt_eval_cache("database disk image is malformed (in '/tmp/other.sqlite')")
                .is_none()
        );
    }

    #[test]
    fn corrupt_eval_cache_detects_a_blob_with_no_schema() {
        let fp = "fcafabf48d74fdfcd38e0c9d903fe9caa423e4ba9404fe07f5a1a8a5d2ab0253";
        let msg = format!(
            "failed to evaluate 'packages': Evaluation error: [nix::SQLiteError] executing SQLite \
             query 'select rowid, type, value, context from Attributes where parent = 0 and name \
             = '': SQL logic error, no such table: Attributes \
             (in '/var/lib/gradient-worker/eval-cache/eval-cache-v6/{fp}.sqlite')"
        );

        assert!(is_corrupt_eval_cache_error(&msg));
        assert_eq!(corrupt_eval_cache(&msg).unwrap().fingerprint, fp);
    }

    #[test]
    fn a_schemaless_blob_is_not_shareable() {
        let mut header_only = b"SQLite format 3\0".to_vec();
        header_only.resize(4096, 0);

        assert!(!is_shareable_eval_cache(&header_only));
        assert!(!is_shareable_eval_cache(b""));
        assert!(!is_shareable_eval_cache(b"not a database at all"));
    }

    #[test]
    fn a_blob_with_a_schema_is_shareable() {
        let mut blob = b"SQLite format 3\0".to_vec();
        blob.resize(4096, 0);
        blob.extend_from_slice(
            b"CREATE TABLE Attributes (parent integer, name text, type integer, value text)",
        );
        blob.resize(8192, 0);

        assert!(is_shareable_eval_cache(&blob));
    }

    #[test]
    fn a_missing_table_outside_the_eval_cache_is_not_healed() {
        assert!(
            corrupt_eval_cache(
                "[nix::SQLiteError] ...: SQL logic error, no such table: Attributes \
                 (in '/nix/var/nix/db/db.sqlite')"
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn corrupt_eval_cache_fails_typed_without_reporting() {
        let repo = "https://example.com/repo";
        let fp = "deadbeefcafebabe1234567890abcdef1234567890abcdef1234567890abcdef";
        let corrupt = AttrError {
            attr: "packages".into(),
            message: format!(
                "database disk image is malformed \
                 (in '/var/lib/gradient-worker/eval-cache/eval-cache-v6/{fp}.sqlite')"
            ),
        };
        let resolver = FakeDerivationResolver::new().with_flake_errors(repo, vec![corrupt]);
        let drv_reader = FakeDrvReader::new();
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        let err = evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .expect_err("a corrupt eval-cache must fail, not complete");

        let corrupt = err
            .downcast_ref::<CorruptEvalCache>()
            .expect("must be a typed CorruptEvalCache for the self-heal");
        assert_eq!(corrupt.fingerprint, fp);
        assert!(
            reporter.last_eval_result().is_none(),
            "SQLite corruption noise must not be reported as the eval result"
        );
    }

    #[test]
    fn unmatched_explicit_target_errors_but_wildcard_is_silent() {
        assert_eq!(
            unmatched_target_errors(&["packages.x86_64-linux.uxc".to_string()]),
            vec![
                "target 'packages.x86_64-linux.uxc' matched no derivations in the flake"
                    .to_string()
            ]
        );
        assert!(unmatched_target_errors(&["packages.x86_64-linux.#".to_string()]).is_empty());
        assert!(unmatched_target_errors(&["packages.x86_64-linux.*".to_string()]).is_empty());
        assert!(unmatched_target_errors(&["!nixosConfigurations.foo".to_string()]).is_empty());
    }

    fn make_flake_job(repo: &str) -> FlakeJob {
        FlakeJob {
            steps: vec![],
            source: FlakeSource::Repository {
                url: repo.into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        }
    }

    fn never_abort() -> crate::executor::AbortSignal {
        crate::executor::AbortSignal::never()
    }

    #[test]
    fn explicit_attr_set_keeps_only_wildcard_free_includes() {
        let set = explicit_attr_set(&[
            "packages.x86_64-linux.hello".into(),
            "packages.x86_64-linux.#".into(),
            "packages.x86_64-linux.*".into(),
            "!packages.x86_64-linux.broken".into(),
            "checks.\"py.3\".unit".into(),
        ]);

        assert!(set.contains("packages.x86_64-linux.hello"));
        assert!(
            set.contains("checks.py.3.unit"),
            "quoted dots collapse to the discovered path form"
        );
        assert!(
            !set.contains("packages.x86_64-linux.broken"),
            "exclusions are not explicit requests"
        );
        assert_eq!(set.len(), 2, "wildcard patterns contribute nothing");
    }

    fn setup_from_fixture(
        fixture: &StoreFixture,
        repo: &str,
        attr: &str,
    ) -> (FakeDerivationResolver, FakeDrvReader) {
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs(repo, vec![attr.to_string()])
            .with_drv_path(repo, attr, fixture.entry_point.clone());

        let drv_reader = FakeDrvReader::from_raw_drvs(fixture.raw_drvs.clone());

        (resolver, drv_reader)
    }

    #[test]
    fn cached_source_requires_store_path_present() {
        let cached = FlakeSource::Cached {
            store_path: "/nix/store/abc-source".into(),
        };
        assert_eq!(
            required_local_source(&cached),
            Some("/nix/store/abc-source")
        );
        let repo = FlakeSource::Repository {
            url: "u".into(),
            commit: "c".into(),
        };
        assert_eq!(required_local_source(&repo), None);
    }

    #[tokio::test]
    async fn test_eval_closure_walk_empty_store() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        assert!(reporter.len() >= 2);

        let all = reporter.all_eval_derivations();
        assert_eq!(all.len(), fixture.derivations.len());
        let entry = all
            .iter()
            .find(|d| d.drv_path == fixture.entry_point)
            .unwrap();
        assert_eq!(entry.attr, "hello");
        if let ReportedEvent::EvalResult { warnings, .. } = reporter.last_eval_result().unwrap() {
            assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);
        }
    }

    #[tokio::test]
    async fn the_evaluators_are_released_before_the_closure_walk() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        assert_eq!(resolver.releases(), 1);
    }

    #[tokio::test]
    async fn pushes_batch_closure_before_reporting_it() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let events = reporter.events();
        let mut pushed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for event in &events {
            match event {
                ReportedEvent::PathsPushed { paths } => {
                    pushed.extend(paths.iter().map(|(path, _)| path.as_str()));
                }
                ReportedEvent::EvalResult { derivations, .. } => {
                    for d in derivations {
                        assert!(
                            pushed.contains(d.drv_path.as_str()),
                            "reported {} before pushing it",
                            d.drv_path
                        );
                        for src in &d.input_sources {
                            assert!(
                                pushed.contains(src.as_str()),
                                "reported {} before pushing its source {src}",
                                d.drv_path
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        assert!(!pushed.is_empty(), "expected at least one push");
    }

    #[tokio::test]
    async fn each_batch_is_pushed_before_its_report_and_a_shared_source_once() {
        let reporter = RecordingJobReporter::new();
        let (flushes, published) = mpsc::channel(PUBLISH_BACKLOG);
        for (drv, warning) in [("a.drv", "first"), ("b.drv", "second")] {
            flushes
                .send(Flush {
                    paths: vec![(drv.into(), Some(120)), ("builder.sh".into(), None)],
                    derivations: vec![],
                    warnings: vec![warning.into()],
                })
                .await
                .unwrap();
        }
        drop(flushes);

        publish(&reporter, published).await.unwrap();

        let events = reporter.events();
        let [
            ReportedEvent::PathsPushed { paths: p1 },
            ReportedEvent::EvalResult { warnings: w1, .. },
            ReportedEvent::PathsPushed { paths: p2 },
            ReportedEvent::EvalResult { warnings: w2, .. },
        ] = events.as_slice()
        else {
            panic!("a push before each report: {events:?}");
        };
        assert_eq!(
            p1,
            &[
                ("a.drv".to_owned(), Some(120)),
                ("builder.sh".to_owned(), None)
            ]
        );
        assert_eq!(p2, &[("b.drv".to_owned(), Some(120))]);
        assert_eq!([w1, w2].map(|w| w[0].as_str()), ["first", "second"]);
    }

    #[test]
    fn a_drv_nar_size_is_its_framed_contents() {
        fn framed(len: usize) -> usize {
            8 + len.div_ceil(8) * 8
        }
        for len in [0, 1, 7, 8, 9, 3612] {
            let frame: usize = ["nix-archive-1", "(", "type", "regular", "contents"]
                .iter()
                .map(|s| framed(s.len()))
                .sum::<usize>()
                + framed(len)
                + framed(")".len());
            assert_eq!(drv_nar_size(len), frame as u64, "len {len}");
        }
    }

    #[tokio::test]
    async fn a_known_dependency_is_neither_reported_nor_walked() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let known: Vec<String> = fixture
            .derivations
            .iter()
            .map(|d| d.drv_path.clone())
            .filter(|p| *p != fixture.entry_point)
            .collect();
        let mut reporter = RecordingJobReporter::new().with_known_drv_paths(known);

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let all = reporter.all_eval_derivations();
        assert_eq!(all.len(), 1, "only the entry point is walked: {all:?}");
        assert_eq!(all[0].drv_path, fixture.entry_point);
        assert!(
            !all[0].dependencies.is_empty(),
            "the pruned deps stay named"
        );
        let pushed = reporter.all_pushed_paths();
        let drvs: Vec<&String> = pushed.iter().filter(|p| p.ends_with(".drv")).collect();
        assert_eq!(
            drvs,
            vec![&fixture.entry_point],
            "only the walked drv is pushed"
        );
        for src in &all[0].input_sources {
            assert!(pushed.contains(src), "its source {src} goes with it");
        }
    }

    #[tokio::test]
    async fn test_eval_empty_attrs() {
        let resolver = FakeDerivationResolver::new();
        let drv_reader = FakeDrvReader::new();
        let job = make_flake_job("https://example.com/empty");
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        if let ReportedEvent::EvalResult { derivations, .. } = reporter.last_eval_result().unwrap()
        {
            assert!(derivations.is_empty());
        } else {
            panic!("expected EvalResult");
        }
    }

    #[tokio::test]
    async fn test_eval_missing_drv_fails_loudly() {
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs("repo", vec!["pkg".into()])
            .with_drv_path("repo", "pkg", "/nix/store/nonexistent.drv");
        let drv_reader = FakeDrvReader::new();
        let job = make_flake_job("repo");
        let mut reporter = RecordingJobReporter::new();

        let err = evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .expect_err("missing .drv must abort the eval");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nonexistent.drv"),
            "error should mention the missing path: {msg}"
        );
        assert!(
            msg.contains("aborting eval"),
            "error should explain the abort: {msg}"
        );
    }

    #[tokio::test]
    async fn test_eval_aborts_when_signal_set_before_start() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        let (tx, rx) = crate::executor::AbortSignal::channel();
        let mut abort = rx;
        tx.send(true).unwrap();

        let err = evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut abort,
        )
        .await
        .expect_err("aborted eval must return Err");
        assert!(
            format!("{err:#}").contains("aborted by server"),
            "error should mention abort: {err:#}"
        );
        assert!(reporter.is_empty(), "reporter should not see any events");
    }

    #[derive(Debug)]
    struct StalledResolver;

    #[async_trait::async_trait]
    impl DerivationResolver for StalledResolver {
        async fn list_flake_derivations(
            &self,
            _: String,
            _: Vec<String>,
            _: &[(String, String)],
            _: &dyn gradient_sources::ImportBuilder,
        ) -> Result<FlakeDiscovery> {
            std::future::pending().await
        }

        async fn release_evaluators(&self) {}

        async fn get_derivation(&self, _: String) -> Result<gradient_derivation::Derivation> {
            std::future::pending().await
        }

        async fn get_features(&self, _: String) -> Result<(String, Vec<String>)> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn abort_interrupts_a_running_nix_evaluation() {
        let fixture = load_store(&fixture_dir());
        let (_, drv_reader) = setup_from_fixture(&fixture, "https://example.com/repo", "hello");
        let job = make_flake_job("https://example.com/repo");
        let mut reporter = RecordingJobReporter::new();
        let (tx, mut abort) = crate::executor::AbortSignal::channel();

        let eval = evaluate_derivations_with(
            &StalledResolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut abort,
        );
        let fire = async {
            tokio::task::yield_now().await;
            tx.send(true).unwrap();
            std::future::pending::<()>().await
        };
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                out = eval => out,
                _ = fire => unreachable!(),
            }
        })
        .await
        .expect("abort must end an evaluation stuck in nix")
        .expect_err("aborted eval must return Err");

        assert!(format!("{err:#}").contains("aborted by server"), "{err:#}");
    }

    async fn evaluate_with(resolver: &FakeDerivationResolver) -> RecordingJobReporter {
        let repo = "https://example.com/repo";
        let mut reporter = RecordingJobReporter::new();
        evaluate_derivations_with(
            resolver,
            &FakeDrvReader::new(),
            &make_flake_job(repo),
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();
        reporter
    }

    fn failed_attr_messages(reporter: &RecordingJobReporter) -> Vec<(String, String)> {
        let events = reporter.events();
        let result_at = events
            .iter()
            .rposition(|e| matches!(e, ReportedEvent::EvalResult { .. }))
            .expect("an EvalResult");
        events
            .into_iter()
            .take(result_at)
            .filter_map(|e| match e {
                ReportedEvent::EvalMessage {
                    level: EvalMessageLevel::Error,
                    source,
                    message,
                } => Some((source, message)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_resolve_failure_is_reported_under_its_attribute_before_the_result() {
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs("https://example.com/repo", vec!["broken".into()]);

        let messages = failed_attr_messages(&evaluate_with(&resolver).await);

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert_eq!(messages[0].0, "nix-eval:broken");
        assert!(messages[0].1.contains("broken"), "{messages:?}");
    }

    #[tokio::test]
    async fn a_discovery_failure_is_reported_under_its_attribute_before_the_result() {
        let resolver = FakeDerivationResolver::new().with_flake_errors(
            "https://example.com/repo",
            vec![AttrError {
                attr: "nixosConfigurations.host".into(),
                message: "boom".into(),
            }],
        );

        let messages = failed_attr_messages(&evaluate_with(&resolver).await);

        assert_eq!(
            messages,
            vec![(
                "nix-eval:nixosConfigurations.host".to_owned(),
                "failed to evaluate 'nixosConfigurations.host': boom".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn test_eval_dependencies_match_fixture() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let all = reporter.all_eval_derivations();
        {
            let eval_deps: std::collections::HashMap<&str, Vec<&str>> = all
                .iter()
                .map(|d| {
                    (
                        d.drv_path.as_str(),
                        d.dependencies.iter().map(|s| s.as_str()).collect(),
                    )
                })
                .collect();

            for drv in &fixture.derivations {
                let eval_dep_list = eval_deps
                    .get(drv.drv_path.as_str())
                    .unwrap_or_else(|| panic!("missing {} in eval result", drv.drv_path));
                let fixture_dep_list = fixture.tree.get(&drv.drv_path).unwrap();

                let mut eval_sorted: Vec<&str> = eval_dep_list.clone();
                eval_sorted.sort();
                let mut fixture_sorted: Vec<&str> =
                    fixture_dep_list.iter().map(|s| s.as_str()).collect();
                fixture_sorted.sort();

                assert_eq!(
                    eval_sorted, fixture_sorted,
                    "dependency mismatch for {}",
                    drv.drv_path
                );
            }
        }
    }
}
