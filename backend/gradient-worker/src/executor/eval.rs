/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::dependencies::{EvalDependencySource, Imports};
use super::eval_cache_sharing::{EVAL_CACHE_FLUSH_INTERVAL, share_eval_cache};
use super::progress_report::{ChangeReporter, thunk_progress};
use crate::nix::wildcard_walk;
use crate::worker_pool::WorkerPoolResolver;
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use futures::stream::{FuturesOrdered, FuturesUnordered, StreamExt as _};
use gradient_derivation::parse_drv;
use gradient_eval::pool::MemoryGuard;
use gradient_sources::{
    AttrError, DerivationResolver, FlakeDiscovery, Resolved, ResolvedDerivation, ShardSpec,
};
use gradient_wire::messages::{
    DiscoveredDerivation, EvalAttrCost, EvalStatsReport, FlakeJob, FlakeOutputNode, FlakeSource,
};
use gradient_wire::types::{
    EvalMessageLevel, License, RunnerSpec, attr_eval_source, runner_image_attr,
};
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
            let (exclude, segs) = wildcard_walk::parse_pattern(w);
            (!exclude && !segs.iter().any(|s| s.contains('*')))
                .then(|| wildcard_walk::segments_to_pattern(&segs))
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
const KNOWN_QUERIES_IN_FLIGHT: usize = 2;

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
        fork_workers: NonZeroUsize,
        max_eval_rss: u64,
        memory: MemoryGuard,
        eval_cache_dir: String,
        eval_cache_share: bool,
        eval_metrics: bool,
    ) -> Self {
        let resolver = Arc::new(WorkerPoolResolver::new(
            fork_workers.get(),
            max_eval_rss,
            memory,
            eval_cache_dir,
            gradient_util::trace_file::active_dir().map(|dir| dir.to_string_lossy().into_owned()),
            eval_metrics,
        ));

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
    let mut sharing = match eval_cache_fingerprint(evaluator, repo, &eval_overrides).await {
        Some(fingerprint) => {
            let path = PathBuf::from(eval_cache_path(evaluator, &fingerprint));
            share_eval_cache(path, updater, fingerprint).await
        }
        None => None,
    };

    let thunks = evaluator.resolver.live_thunks();
    thunks.reset();
    let sink = updater.eval_progress_sink();
    let snapshot = || thunk_progress(thunks.total());
    let mut reporter = ChangeReporter::default();
    let evaluation = async {
        tokio::select! {
            evaluated = evaluate_derivations_with(
                &*evaluator.resolver,
                &FsDrvReader,
                job,
                local_flake_path,
                updater,
                abort,
            ) => evaluated,
            never = reporter.run(&*sink, snapshot) => match never {},
        }
    };
    let evaluated = match sharing.as_mut() {
        Some(sharing) => {
            sharing
                .beside_flusher(EVAL_CACHE_FLUSH_INTERVAL, evaluation)
                .await
        }
        None => evaluation.await,
    };
    reporter.flush(&*sink, &snapshot).await;
    let EvalOutcome { flake_nodes } = match evaluated {
        Ok(outcome) => outcome,
        Err(e) => {
            if let Some(corrupt) = e.downcast_ref::<CorruptEvalCache>() {
                drop(sharing);
                warn!(fingerprint = %corrupt.fingerprint, "eval-cache corrupt; dropping local blob + recycling eval workers");
                delete_eval_cache_blob(&eval_cache_path(evaluator, &corrupt.fingerprint)).await;
                evaluator.resolver.release_evaluators().await;
            }
            return Err(e);
        }
    };

    let totals = evaluator.resolver.take_eval_stats();
    if totals.total_thunks > 0 || !totals.per_entry_point.is_empty() {
        let report = build_eval_stats_report(
            totals,
            flake_nodes,
            u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        if let Err(e) = updater.report_eval_stats(report).await {
            warn!(error = %e, "failed to send eval stats report");
        }
    }

    if let Some(sharing) = sharing.as_mut() {
        sharing.flush().await;
    }

    Ok(())
}

pub(super) fn eval_cache_path(evaluator: &WorkerEvaluator, fingerprint: &str) -> String {
    format!(
        "{}/eval-cache-v6/{fingerprint}.sqlite",
        evaluator.resolver.eval_cache_dir()
    )
}

pub(super) async fn eval_cache_fingerprint(
    evaluator: &WorkerEvaluator,
    repo: String,
    overrides: &[(String, String)],
) -> Option<String> {
    if !evaluator.eval_cache_share {
        return None;
    }

    evaluator
        .resolver
        .fingerprint(repo, overrides)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "eval-cache fingerprint failed; evaluating local-only");
            None
        })
}

pub(super) fn build_flake_url(job: &FlakeJob, local_flake_path: Option<&str>) -> String {
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

pub(super) fn eval_input_overrides(
    job: &FlakeJob,
    local_flake_path: Option<&str>,
) -> Vec<(String, String)> {
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
    wave: &[Queued],
) -> Result<Vec<(gradient_derivation::Derivation, u64)>> {
    let mut futs: FuturesUnordered<_> = wave
        .iter()
        .enumerate()
        .map(|(i, queued)| {
            let drv_path = queued.drv_path().to_owned();
            async move {
                let bytes = drv_reader.read_drv(&drv_path).await.with_context(|| {
                    format!(
                        "read .drv {drv_path} during closure walk; aborting eval \
                         to avoid silently dropping dependencies"
                    )
                })?;
                let parsed = parse_drv(&bytes).with_context(|| {
                    format!(
                        "parse .drv {drv_path} during closure walk; aborting eval \
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
}

async fn publish(
    reporter: &dyn JobReporter,
    mut flushes: mpsc::Receiver<Flush>,
) -> Result<HashSet<String>> {
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

    Ok(pushed)
}

fn fresh_paths(pushed: &mut HashSet<String>, flush: &mut Flush) -> Vec<(String, Option<u64>)> {
    std::mem::take(&mut flush.paths)
        .into_iter()
        .filter(|(path, _)| pushed.insert(path.clone()))
        .collect()
}

async fn report(reporter: &dyn JobReporter, flush: Flush) -> Result<()> {
    reporter
        .report_eval_result(flush.derivations, vec![], vec![])
        .await
}

fn drv_nar_size(len: usize) -> u64 {
    (112 + len.div_ceil(8) * 8) as u64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Root {
    pub(super) drv_path: String,
    pub(super) attr: String,
    pub(super) ifd: bool,
    pub(super) licenses: Vec<License>,
    pub(super) runner: Option<RunnerSpec>,
}

enum Queued {
    Root(Box<Root>),
    Dependency(String),
}

impl Queued {
    fn drv_path(&self) -> &str {
        match self {
            Queued::Root(root) => &root.drv_path,
            Queued::Dependency(drv_path) => drv_path,
        }
    }

    fn discovered(
        self,
        drv: &gradient_derivation::Derivation,
        produced: Vec<String>,
    ) -> DiscoveredDerivation {
        let mut discovered = match self {
            Queued::Root(root) => {
                let Root {
                    drv_path,
                    attr,
                    ifd,
                    licenses,
                    runner,
                } = *root;
                DiscoveredDerivation {
                    ifd,
                    licenses,
                    runner,
                    ..gradient_derivation::discovered_derivation(Some(attr), drv_path, drv)
                }
            }
            Queued::Dependency(drv_path) => {
                gradient_derivation::discovered_derivation(None, drv_path, drv)
            }
        };
        discovered.dependencies.extend(produced);
        discovered
    }
}

pub(super) async fn walk_and_publish(
    imports: &Imports<'_>,
    roots: Vec<Root>,
    abort: &mut super::AbortSignal,
) -> Result<()> {
    let (flushes, published) = mpsc::channel(PUBLISH_BACKLOG);
    let walk = async {
        let mut walker = ClosureWalker::new(imports.clone(), flushes);
        roots.into_iter().for_each(|root| walker.add_root(root));
        while walker.has_work_left() {
            walker.advance_walk(abort).await?;
        }
        walker.completed();
        walker.flush().await
    };
    tokio::try_join!(walk, publish(imports.reporter, published))?;
    Ok(())
}

#[derive(Default)]
struct FoundAttrs {
    attrs: usize,
    roots: Vec<Root>,
    failed: Vec<AttrError>,
}

impl FoundAttrs {
    fn take(&mut self, (attr, resolved): ResolvedDerivation) -> Vec<Root> {
        self.attrs += 1;
        match resolved {
            Ok(Resolved {
                drv_path,
                licenses,
                runner,
            }) => {
                let image = runner
                    .as_ref()
                    .and_then(|spec| spec.image.clone())
                    .map(|image| Root {
                        drv_path: image,
                        attr: runner_image_attr(&attr),
                        ifd: false,
                        licenses: vec![],
                        runner: None,
                    });
                let program = Root {
                    drv_path,
                    attr,
                    ifd: false,
                    licenses,
                    runner,
                };
                let roots: Vec<Root> = std::iter::once(program).chain(image).collect();
                self.roots.extend(roots.iter().cloned());
                roots
            }
            Err(e) => {
                self.failed.push(AttrError {
                    attr,
                    message: format!("{e:#}"),
                });
                vec![]
            }
        }
    }
}

type KnownAnswer = (Vec<String>, HashSet<String>);

struct ClosureWalker<'a> {
    imports: Imports<'a>,
    known_queries: FuturesOrdered<BoxFuture<'a, KnownAnswer>>,
    batch: Vec<DiscoveredDerivation>,
    visited: HashSet<String>,
    rooted: HashSet<String>,
    pending: VecDeque<Queued>,
    walked: usize,
    start: Instant,
    paths: Vec<(String, Option<u64>)>,
    flushes: mpsc::Sender<Flush>,
}

impl<'a> ClosureWalker<'a> {
    fn new(imports: Imports<'a>, flushes: mpsc::Sender<Flush>) -> Self {
        Self {
            imports,
            known_queries: FuturesOrdered::new(),
            batch: Vec::new(),
            visited: HashSet::new(),
            rooted: HashSet::new(),
            pending: VecDeque::new(),
            walked: 0,
            start: Instant::now(),
            paths: Vec::new(),
            flushes,
        }
    }

    fn add_root(&mut self, root: Root) {
        self.rooted.insert(root.drv_path.clone());
        self.visited.insert(root.drv_path.clone());
        self.pending.push_front(Queued::Root(Box::new(root)));
    }

    async fn walk_found(
        mut self,
        attrs: &mut mpsc::UnboundedReceiver<ResolvedDerivation>,
        abort: &super::AbortSignal,
    ) -> Result<(FoundAttrs, Flush)> {
        let mut found = FoundAttrs::default();
        loop {
            let next = if self.has_work_left() {
                attrs.try_recv().ok()
            } else {
                attrs.recv().await
            };
            match next {
                Some(attr) => {
                    for root in found.take(attr) {
                        self.add_root(root);
                    }
                }
                None if !self.has_work_left() => break,
                None => self.advance_walk(abort).await?,
            }
        }
        self.completed();

        let rest = Flush {
            paths: std::mem::take(&mut self.paths),
            derivations: std::mem::take(&mut self.batch),
        };
        Ok((found, rest))
    }

    fn has_work_left(&self) -> bool {
        !self.pending.is_empty() || !self.known_queries.is_empty()
    }

    async fn advance_walk(&mut self, abort: &super::AbortSignal) -> Result<()> {
        if abort.is_aborted() {
            return Err(abort_err());
        }
        if self.known_queries.len() < KNOWN_QUERIES_IN_FLIGHT {
            let wave = self.next_wave();
            if !wave.is_empty() {
                return self.process_wave(wave).await;
            }
        }
        if let Some(answer) = self.known_queries.next().await {
            self.enqueue_unknown(answer);
        }
        Ok(())
    }

    fn completed(&self) {
        info!(
            walked = self.walked,
            elapsed_secs = self.start.elapsed().as_secs(),
            "closure walk complete"
        );
    }

    async fn flush(&mut self) -> Result<()> {
        debug!(
            count = self.batch.len(),
            remaining = self.pending.len(),
            "flushing eval batch"
        );
        let flush = Flush {
            paths: std::mem::take(&mut self.paths),
            derivations: std::mem::take(&mut self.batch),
        };
        self.flushes
            .send(flush)
            .await
            .map_err(|_| anyhow::anyhow!("the eval publisher stopped before the walk"))
    }

    fn next_wave(&mut self) -> Vec<Queued> {
        let mut wave = Vec::with_capacity(self.pending.len().min(DRV_READ_CONCURRENCY));
        while wave.len() < DRV_READ_CONCURRENCY
            && let Some(queued) = self.pending.pop_back()
        {
            match &queued {
                Queued::Dependency(drv_path) if self.rooted.contains(drv_path) => {}
                _ => wave.push(queued),
            }
        }
        wave
    }

    #[tracing::instrument(name = "wave", level = "debug", skip_all, fields(pending = self.pending.len()))]
    async fn process_wave(&mut self, wave: Vec<Queued>) -> Result<()> {
        let parsed_drvs = self.parse_while_asking(&wave).await?;
        let produced = produced_drvs(&self.imports, &parsed_drvs).await?;
        let new_deps = self.unvisited_inputs(&parsed_drvs, &produced);
        self.ask_known(new_deps);
        self.record_wave(wave, parsed_drvs, produced).await
    }

    async fn parse_while_asking(
        &mut self,
        wave: &[Queued],
    ) -> Result<Vec<(gradient_derivation::Derivation, u64)>> {
        let drv_reader = self.imports.drv_reader;
        let parse = parse_drv_wave(drv_reader, wave);
        tokio::pin!(parse);
        loop {
            tokio::select! {
                parsed = &mut parse => return parsed,
                Some(answer) = self.known_queries.next(), if !self.known_queries.is_empty() => {
                    self.enqueue_unknown(answer);
                }
            }
        }
    }

    fn unvisited_inputs(
        &mut self,
        parsed: &[(gradient_derivation::Derivation, u64)],
        produced: &[Vec<String>],
    ) -> Vec<String> {
        let inputs = parsed
            .iter()
            .flat_map(|(drv, _)| drv.input_derivations.iter().map(|input| &input.path))
            .chain(produced.iter().flatten());
        let mut new_deps: Vec<String> = inputs
            .filter(|path| !self.visited.contains(path.as_str()))
            .cloned()
            .collect();
        new_deps.sort_unstable();
        new_deps.dedup();
        self.visited.extend(new_deps.iter().cloned());
        new_deps
    }

    fn ask_known(&mut self, deps: Vec<String>) {
        let reporter = self.imports.reporter;
        self.known_queries.push_back(Box::pin(async move {
            if deps.is_empty() {
                return (deps, HashSet::new());
            }
            let known: HashSet<String> = reporter
                .query_known_derivations(deps.clone())
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "query_known_derivations failed; treating all as unknown");
                    vec![]
                })
                .into_iter()
                .collect();
            (deps, known)
        }));
    }

    fn enqueue_unknown(&mut self, (asked, known): KnownAnswer) {
        if !known.is_empty() {
            debug!(pruned = known.len(), "closure walk: pruning known subtrees");
        }
        self.pending.extend(
            asked
                .into_iter()
                .filter(|dep| !known.contains(dep))
                .map(Queued::Dependency),
        );
    }

    async fn record_wave(
        &mut self,
        wave: Vec<Queued>,
        parsed_drvs: Vec<(gradient_derivation::Derivation, u64)>,
        produced: Vec<Vec<String>>,
    ) -> Result<()> {
        for ((queued, (drv, nar_size)), produced) in wave.into_iter().zip(parsed_drvs).zip(produced)
        {
            self.paths
                .push((queued.drv_path().to_owned(), Some(nar_size)));
            self.paths
                .extend(drv.input_sources.iter().map(|src| (src.clone(), None)));
            self.batch.push(queued.discovered(&drv, produced));
            self.walked += 1;

            if self.walked.is_multiple_of(500) {
                info!(
                    walked = self.walked,
                    pending = self.pending.len(),
                    elapsed_secs = self.start.elapsed().as_secs(),
                    "closure walk progress"
                );
            }

            if self.batch.len() >= EVAL_BATCH_SIZE {
                self.flush().await?;
            }
        }

        Ok(())
    }
}

async fn produced_drvs(
    imports: &Imports<'_>,
    parsed: &[(gradient_derivation::Derivation, u64)],
) -> Result<Vec<Vec<String>>> {
    futures::future::try_join_all(
        parsed
            .iter()
            .map(|(drv, _)| imports.produced_by_inputs(drv)),
    )
    .await
    .map_err(|e| anyhow::anyhow!("resolving a dynamic derivation input failed: {e}"))
}

pub(super) async fn report_failed_attrs(
    updater: &mut dyn JobReporter,
    failed: &[AttrError],
) -> Result<()> {
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

fn flake_nodes_from_roots(roots: &[Root]) -> Vec<FlakeOutputNode> {
    roots
        .iter()
        .map(|Root { attr, drv_path, .. }| {
            let mut segs = wildcard_walk::parse_pattern(attr).1;
            let name = segs.pop().unwrap_or_default();
            let parent = (!segs.is_empty()).then(|| wildcard_walk::segments_to_pattern(&segs));

            FlakeOutputNode {
                path: attr.clone(),
                parent,
                name,
                kind: flake_kind(attr).to_string(),
                is_derivation: true,
                drv_path: Some(drv_path.clone()),
            }
        })
        .collect()
}

pub(super) fn build_eval_stats_report(
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
                thunks: c.cost.thunks,
                fn_calls: c.cost.fn_calls,
                eval_ms: c.cost.eval_ms,
                alloc_bytes: c.cost.alloc_bytes,
            })
            .collect(),
        flake_nodes,
        sets: totals.sets,
        subtrees: totals.subtrees,
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

    let streamed = discover_and_walk(
        resolver,
        drv_reader,
        job,
        local_flake_path,
        &*updater,
        abort,
        Listing::Wildcards,
    )
    .await?;
    let listed = match streamed.listed {
        Ok(listed) => listed,
        Err(e) if e.is::<crate::executor::failure::JobAborted>() => return Err(e),
        Err(e) => {
            let err_msg = format!("list_flake_derivations failed: {:#}", e);
            warn!(error = %err_msg, "reporting eval error to server");
            let _ = updater
                .report_eval_result(vec![], vec![], vec![err_msg])
                .await;
            return Err(e).context("run list_flake_derivations");
        }
    };

    finish_evaluation(updater, job, listed, streamed.found, streamed.rest).await
}

pub(super) enum Listing {
    Wildcards,
    Given {
        shards: Vec<ShardSpec>,
        excludes: Vec<String>,
    },
}

pub(super) struct ShardOutcome {
    pub(super) deferred: Vec<ShardSpec>,
    pub(super) flake_nodes: Vec<FlakeOutputNode>,
}

pub(super) async fn plan_shards_with(
    resolver: &dyn DerivationResolver,
    drv_reader: &dyn DrvReader,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    reporter: &dyn JobReporter,
    abort: &super::AbortSignal,
) -> Result<(Vec<ShardSpec>, Vec<AttrError>)> {
    let imports = Imports::new(reporter, drv_reader, abort.clone());
    let dependencies = EvalDependencySource::new(imports);
    let overrides = eval_input_overrides(job, local_flake_path);
    let planned = resolver.plan_shards(
        build_flake_url(job, local_flake_path),
        job.wildcards.clone(),
        &overrides,
        &dependencies,
    );

    unless_aborted(&mut abort.clone(), planned).await
}

#[expect(
    clippy::too_many_arguments,
    reason = "the same evaluation inputs as a whole evaluation, plus the shard"
)]
pub(super) async fn evaluate_shard_with(
    resolver: &dyn DerivationResolver,
    drv_reader: &dyn DrvReader,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    updater: &mut dyn JobReporter,
    abort: &super::AbortSignal,
    shards: Vec<ShardSpec>,
    excludes: Vec<String>,
) -> Result<ShardOutcome> {
    let listing = Listing::Given { shards, excludes };
    let streamed = discover_and_walk(
        resolver,
        drv_reader,
        job,
        local_flake_path,
        &*updater,
        abort,
        listing,
    )
    .await?;
    let FlakeDiscovery {
        warnings,
        errors,
        deferred,
    } = streamed.listed?;
    let mut failed = errors;
    failed.extend(streamed.found.failed);
    report_rest(updater, failed, warnings, Vec::new(), streamed.rest).await?;

    Ok(ShardOutcome {
        deferred,
        flake_nodes: flake_nodes_from_roots(&streamed.found.roots),
    })
}

struct Streamed {
    listed: Result<FlakeDiscovery>,
    found: FoundAttrs,
    rest: Flush,
}

async fn discover_and_walk(
    resolver: &dyn DerivationResolver,
    drv_reader: &dyn DrvReader,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    reporter: &dyn JobReporter,
    abort: &super::AbortSignal,
    listing: Listing,
) -> Result<Streamed> {
    let (found_tx, mut found_rx) = mpsc::unbounded_channel();
    let (flushes, published) = mpsc::channel(PUBLISH_BACKLOG);
    let imports = Imports::new(reporter, drv_reader, abort.clone());
    let discovery = async {
        Ok(discover(
            resolver,
            &imports,
            job,
            local_flake_path,
            abort.clone(),
            found_tx,
            listing,
        )
        .await)
    };
    let walk = ClosureWalker::new(imports.clone(), flushes).walk_found(&mut found_rx, abort);
    let ((listed, (found, mut rest)), mut pushed) = tokio::try_join!(
        async { tokio::try_join!(discovery, walk) },
        publish(reporter, published),
    )?;

    rest.paths = fresh_paths(&mut pushed, &mut rest);
    Ok(Streamed {
        listed,
        found,
        rest,
    })
}

async fn discover(
    resolver: &dyn DerivationResolver,
    imports: &Imports<'_>,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    mut abort: super::AbortSignal,
    found: mpsc::UnboundedSender<ResolvedDerivation>,
    listing: Listing,
) -> Result<FlakeDiscovery> {
    let repo = build_flake_url(job, local_flake_path);
    let eval_overrides = eval_input_overrides(job, local_flake_path);
    debug!(repo = %repo, "listing flake derivations");

    let imports = EvalDependencySource::new(imports.clone());
    let send = move |resolved: Vec<ResolvedDerivation>| {
        for derivation in resolved {
            let _ = found.send(derivation);
        }
    };
    let releases = matches!(listing, Listing::Wildcards);
    let list = async {
        match listing {
            Listing::Wildcards => {
                resolver
                    .list_flake_derivations(
                        repo,
                        job.wildcards.clone(),
                        &eval_overrides,
                        &imports,
                        &send,
                    )
                    .await
            }
            Listing::Given { shards, excludes } => {
                resolver
                    .list_given_shards(repo, shards, &excludes, &eval_overrides, &imports, &send)
                    .await
            }
        }
    };
    let listed = unless_aborted(&mut abort, list).await;
    drop(send);

    if releases && listed.is_ok() {
        resolver.release_evaluators().await;
    }
    listed
}

async fn finish_evaluation(
    updater: &mut dyn JobReporter,
    job: &FlakeJob,
    listed: FlakeDiscovery,
    found: FoundAttrs,
    rest: Flush,
) -> Result<EvalOutcome> {
    let FlakeDiscovery {
        warnings,
        errors: mut failed,
        ..
    } = listed;
    failed.extend(found.failed);
    let errors = if found.attrs == 0 {
        warn!("no derivations found for evaluation");
        unmatched_target_errors(&job.wildcards)
    } else {
        if found.roots.is_empty() {
            warn!("all attr resolutions failed");
        }
        Vec::new()
    };
    report_rest(updater, failed, warnings, errors, rest).await?;

    Ok(EvalOutcome {
        flake_nodes: flake_nodes_from_roots(&found.roots),
    })
}

async fn report_rest(
    updater: &mut dyn JobReporter,
    failed: Vec<AttrError>,
    mut warnings: Vec<String>,
    errors: Vec<String>,
    rest: Flush,
) -> Result<()> {
    if let Some(corrupt) = failed.iter().find_map(|e| corrupt_eval_cache(&e.message)) {
        warn!(fingerprint = %corrupt.fingerprint, "eval-cache corrupt; failing for self-heal");
        return Err(anyhow::Error::new(corrupt));
    }

    report_failed_attrs(updater, &failed).await?;
    warnings.sort_unstable();
    warnings.dedup();
    if !rest.paths.is_empty() {
        updater.push_paths(&rest.paths).await?;
    }

    updater
        .report_eval_result(rest.derivations, warnings, errors)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::fakes::derivation_resolver::FakeDerivationResolver;
    use gradient_test_support::prelude::*;
    use gradient_wire::types::{ImportOutcome, Realisation};
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
        assert!(unmatched_target_errors(&["packages.x86_64-linux.*".to_string()]).is_empty());
        assert!(unmatched_target_errors(&["packages.x86_64-linux.**".to_string()]).is_empty());
        assert!(unmatched_target_errors(&["packages.x86_64-*.hello".to_string()]).is_empty());
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
            "packages.x86_64-linux.*".into(),
            "packages.x86_64-linux.**".into(),
            "packages.x86_64-*.hello".into(),
            "!packages.x86_64-linux.broken".into(),
            "checks.\"py.3\".unit".into(),
        ]);

        assert!(set.contains("packages.x86_64-linux.hello"));
        assert!(
            set.contains(r#"checks."py.3".unit"#),
            "quoted dots keep the discovered path form"
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
    async fn the_evaluators_are_released_once_discovery_ends() {
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
    async fn attributes_on_a_derivation_yield_entry_points() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs(repo, vec!["pkg".to_owned(), "legacyPkg".to_owned()])
            .with_drv_path(repo, "pkg", fixture.entry_point.clone())
            .with_drv_path(repo, "legacyPkg", fixture.entry_point.clone());
        let drv_reader = FakeDrvReader::from_raw_drvs(fixture.raw_drvs.clone());
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

        let reported: Vec<(String, String)> = reporter
            .events()
            .into_iter()
            .filter_map(|event| match event {
                ReportedEvent::EvalResult { derivations, .. } => Some(derivations),
                _ => None,
            })
            .flatten()
            .map(|d| (d.attr, d.drv_path))
            .collect();

        let attrs: std::collections::HashSet<&str> = reported
            .iter()
            .map(|(attr, _)| attr.as_str())
            .filter(|attr| !attr.is_empty())
            .collect();
        assert_eq!(
            attrs,
            std::collections::HashSet::from(["pkg", "legacyPkg"]),
            "{reported:?}"
        );

        let mut seen = std::collections::HashSet::new();
        let repeated: Vec<&str> = reported
            .iter()
            .filter(|(attr, _)| !attr.is_empty())
            .map(|(_, path)| path.as_str())
            .filter(|path| !seen.insert(*path))
            .collect();
        assert_eq!(repeated, vec![fixture.entry_point.as_str()], "{reported:?}");
    }

    #[tokio::test]
    async fn each_batch_is_pushed_before_its_report_and_a_shared_source_once() {
        let reporter = RecordingJobReporter::new();
        let (flushes, published) = mpsc::channel(PUBLISH_BACKLOG);
        for drv in ["a.drv", "b.drv"] {
            flushes
                .send(Flush {
                    paths: vec![(drv.into(), Some(120)), ("builder.sh".into(), None)],
                    derivations: vec![],
                })
                .await
                .unwrap();
        }
        drop(flushes);

        publish(&reporter, published).await.unwrap();

        let events = reporter.events();
        let [
            ReportedEvent::PathsPushed { paths: p1 },
            ReportedEvent::EvalResult { .. },
            ReportedEvent::PathsPushed { paths: p2 },
            ReportedEvent::EvalResult { .. },
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
            _: &dyn gradient_sources::EvalDependencies,
            _: gradient_sources::FoundDerivations<'_>,
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

    #[derive(Debug)]
    struct WaitsForTheWalk {
        drv_path: String,
        walked: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl DerivationResolver for WaitsForTheWalk {
        async fn list_flake_derivations(
            &self,
            _: String,
            _: Vec<String>,
            _: &[(String, String)],
            _: &dyn gradient_sources::EvalDependencies,
            found: gradient_sources::FoundDerivations<'_>,
        ) -> Result<FlakeDiscovery> {
            found(vec![(
                "hello".into(),
                Ok(Resolved {
                    drv_path: self.drv_path.clone(),
                    licenses: vec![gpl()],
                    runner: None,
                }),
            )]);
            self.walked.notified().await;
            Ok(FlakeDiscovery::default())
        }

        async fn release_evaluators(&self) {}

        async fn get_derivation(&self, _: String) -> Result<gradient_derivation::Derivation> {
            std::future::pending().await
        }

        async fn get_features(&self, _: String) -> Result<(String, Vec<String>)> {
            std::future::pending().await
        }
    }

    struct SignalsReads {
        drvs: FakeDrvReader,
        walked: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl DrvReader for SignalsReads {
        async fn read_drv(&self, store_path: &str) -> Result<Vec<u8>> {
            self.walked.notify_one();
            self.drvs.read_drv(store_path).await
        }
    }

    #[tokio::test]
    async fn a_root_is_walked_while_discovery_still_runs() {
        let fixture = load_store(&fixture_dir());
        let walked = Arc::new(tokio::sync::Notify::new());
        let resolver = WaitsForTheWalk {
            drv_path: fixture.entry_point.clone(),
            walked: Arc::clone(&walked),
        };
        let drv_reader = SignalsReads {
            drvs: FakeDrvReader::from_raw_drvs(fixture.raw_drvs.clone()),
            walked,
        };
        let mut reporter = RecordingJobReporter::new();

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            evaluate_derivations_with(
                &resolver,
                &drv_reader,
                &make_flake_job("https://example.com/repo"),
                None,
                &mut reporter,
                &mut never_abort(),
            ),
        )
        .await
        .expect("discovery waits for a walk that only a streamed root can start")
        .unwrap();

        assert_eq!(
            reporter.all_eval_derivations().len(),
            fixture.derivations.len()
        );
    }

    fn gpl() -> License {
        License::Spdx {
            id: "GPL-3.0-or-later".into(),
        }
    }

    #[tokio::test]
    async fn only_the_root_carries_its_licenses() {
        let fixture = load_store(&fixture_dir());
        let walked = Arc::new(tokio::sync::Notify::new());
        let resolver = WaitsForTheWalk {
            drv_path: fixture.entry_point.clone(),
            walked: Arc::clone(&walked),
        };
        let drv_reader = SignalsReads {
            drvs: FakeDrvReader::from_raw_drvs(fixture.raw_drvs.clone()),
            walked,
        };
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &make_flake_job("https://example.com/repo"),
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        for d in reporter.all_eval_derivations() {
            let expected = if d.drv_path == fixture.entry_point {
                vec![gpl()]
            } else {
                vec![]
            };
            assert_eq!(d.licenses, expected, "{}", d.drv_path);
        }
    }

    #[derive(Debug)]
    struct ResolvesRunner(Resolved);

    #[async_trait::async_trait]
    impl DerivationResolver for ResolvesRunner {
        async fn list_flake_derivations(
            &self,
            _: String,
            _: Vec<String>,
            _: &[(String, String)],
            _: &dyn gradient_sources::EvalDependencies,
            found: gradient_sources::FoundDerivations<'_>,
        ) -> Result<FlakeDiscovery> {
            found(vec![("apps.x86_64-linux.train".into(), Ok(self.0.clone()))]);
            Ok(FlakeDiscovery::default())
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
    async fn a_runner_root_carries_its_spec_and_its_image_is_a_second_root() {
        let program = numbered_drv(1);
        let image = numbered_drv(2);
        let resolver = ResolvesRunner(Resolved {
            drv_path: program.clone(),
            licenses: vec![],
            runner: Some(gradient_wire::types::RunnerSpec {
                program: "/nix/store/aaaa-train/bin/train".into(),
                image: Some(image.clone()),
                ..Default::default()
            }),
        });
        let drv_reader = FakeDrvReader::new()
            .with_drv(program.clone(), drv_needing(&[]))
            .with_drv(image.clone(), drv_needing(&[]));
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &make_flake_job("https://example.com/repo"),
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let reported = reporter.all_eval_derivations();
        let program_root = reported
            .iter()
            .find(|d| d.drv_path == program)
            .expect("the program root is reported");
        assert_eq!(program_root.attr, "apps.x86_64-linux.train");
        assert!(program_root.runner.is_some());
        let image_root = reported
            .iter()
            .find(|d| d.drv_path == image)
            .expect("the image root is reported");
        assert_eq!(
            image_root.attr,
            "apps.x86_64-linux.train.passthru.gradient.image"
        );
        assert_eq!(image_root.runner, None);
    }

    fn numbered_drv(n: usize) -> String {
        format!("/nix/store/{n:032}-n{n}.drv")
    }

    fn drv_needing(inputs: &[String]) -> Vec<u8> {
        let inputs: Vec<String> = inputs
            .iter()
            .map(|path| format!(r#"("{path}",["out"])"#))
            .collect();
        format!(
            r#"Derive([("out","/nix/store/{:032}-out","","")],[{}],[],"x86_64-linux","/bin/sh",[],[("name","n")])"#,
            0,
            inputs.join(",")
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn the_walk_goes_deep_before_finishing_a_wide_level() {
        let repo = "https://example.com/repo";
        let wide: Vec<String> = (1..=2000).map(numbered_drv).collect();
        let chain: Vec<String> = (9001..=9004).map(numbered_drv).collect();
        let root = numbered_drv(1_000_000);
        let mut drvs: std::collections::HashMap<String, Vec<u8>> = wide
            .iter()
            .map(|path| (path.clone(), drv_needing(&[])))
            .collect();
        for link in chain.windows(2) {
            drvs.insert(link[0].clone(), drv_needing(&link[1..]));
        }
        drvs.insert(chain[3].clone(), drv_needing(&[]));
        let mut needed = wide.clone();
        needed.push(chain[0].clone());
        drvs.insert(root.clone(), drv_needing(&needed));
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs(repo, vec!["root".into()])
            .with_drv_path(repo, "root", root);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &FakeDrvReader::from_raw_drvs(drvs),
            &make_flake_job(repo),
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let order: Vec<String> = reporter
            .all_eval_derivations()
            .iter()
            .map(|d| d.drv_path.clone())
            .collect();
        let at = |path: &String| order.iter().position(|o| o == path).unwrap();
        assert_eq!(order.len(), 2005);
        assert!(
            at(&chain[3]) < at(&wide[0]),
            "the end of the chain waits behind the whole wide level"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_next_wave_is_asked_about_before_the_last_answer_arrives() {
        let parents: Vec<String> = (1..=600).map(numbered_drv).collect();
        let children: Vec<String> = (1001..=1600).map(numbered_drv).collect();
        let root = numbered_drv(1_000_000);
        let mut drvs: std::collections::HashMap<String, Vec<u8>> = parents
            .iter()
            .zip(&children)
            .map(|(parent, child)| (parent.clone(), drv_needing(std::slice::from_ref(child))))
            .collect();
        drvs.extend(
            children
                .iter()
                .map(|child| (child.clone(), drv_needing(&[]))),
        );
        drvs.insert(root.clone(), drv_needing(&parents));
        let reporter =
            RecordingJobReporter::new().with_known_query_delay(std::time::Duration::from_secs(1));

        let reader = FakeDrvReader::from_raw_drvs(drvs);
        walk_and_publish(
            &Imports::new(&reporter, &reader, never_abort()),
            vec![Root {
                drv_path: root.clone(),
                attr: "root".into(),
                ifd: false,
                licenses: vec![],
                runner: None,
            }],
            &mut never_abort(),
        )
        .await
        .unwrap();

        let mut walked: Vec<String> = reporter
            .all_eval_derivations()
            .into_iter()
            .map(|d| d.drv_path)
            .collect();
        walked.sort_unstable();
        let mut expected: Vec<String> = [vec![root], parents, children].concat();
        expected.sort_unstable();
        assert_eq!(
            walked, expected,
            "every derivation is reported exactly once"
        );
        let peak = reporter.peak_known_queries_in_flight();
        assert!(peak > 1, "the walk waited on every answer alone");
        assert!(peak <= KNOWN_QUERIES_IN_FLIGHT, "{peak} queries in flight");
    }

    const DYNAMIC: &str = "/nix/store/ddddddddddddddddddddddddddddddd1-dynamic.drv";
    const MAKER: &str = "/nix/store/ddddddddddddddddddddddddddddddd2-maker.drv";
    const PRODUCED: &str = "/nix/store/ddddddddddddddddddddddddddddddd3-produced.drv";
    const PRODUCED_INPUT: &str = "/nix/store/ddddddddddddddddddddddddddddddd4-input.drv";

    fn leaf_drv(name: &str, inputs: &str) -> Vec<u8> {
        format!(
            r#"Derive([("out","/nix/store/fffffffffffffffffffffffffffffff1-{name}","","")],[{inputs}],[],"x86_64-linux","/bin/sh",[],[("name","{name}")])"#
        )
        .into_bytes()
    }

    fn dynamic_drvs() -> FakeDrvReader {
        FakeDrvReader::new()
            .with_drv(
                DYNAMIC,
                format!(
                    r#"DrvWithVersion("xp-dyn-drv",[("out","","r:sha256","")],[("{MAKER}",([],[("out",["out"])]))],[],"x86_64-linux","/bin/sh",[],[("name","dynamic")])"#
                )
                .into_bytes(),
            )
            .with_drv(
                MAKER,
                br#"Derive([("out","","text:sha256","")],[],[],"x86_64-linux","/bin/sh",[],[("name","maker")])"#.to_vec(),
            )
            .with_drv(PRODUCED, leaf_drv("produced", &format!(r#"("{PRODUCED_INPUT}",["out"])"#)))
            .with_drv(PRODUCED_INPUT, leaf_drv("input", ""))
    }

    #[tokio::test]
    async fn a_dynamic_input_is_built_and_the_derivation_it_produces_is_walked() {
        let realised = Realisation {
            drv_path: MAKER.into(),
            output: "out".into(),
            path: PRODUCED.into(),
        };
        let reporter = RecordingJobReporter::new().with_import_outcome(
            MAKER,
            ImportOutcome::Completed {
                realisations: vec![realised.clone()],
            },
        );
        let reader = dynamic_drvs();

        walk_and_publish(
            &Imports::new(&reporter, &reader, never_abort()),
            vec![Root {
                drv_path: DYNAMIC.into(),
                attr: "dynamic".into(),
                ifd: false,
                licenses: vec![],
                runner: None,
            }],
            &mut never_abort(),
        )
        .await
        .unwrap();

        let walked = reporter.all_eval_derivations();
        let dynamic = walked
            .iter()
            .find(|d| d.drv_path == DYNAMIC)
            .expect("the dynamic derivation is reported");
        assert_eq!(
            dynamic.dependencies,
            vec![MAKER.to_owned(), PRODUCED.to_owned()]
        );
        for path in [PRODUCED, PRODUCED_INPUT] {
            assert!(
                walked.iter().any(|d| d.drv_path == path),
                "{path} is walked: {walked:?}"
            );
        }
        assert!(
            reporter.events().iter().any(|e| matches!(e,
                ReportedEvent::RealisationsRegistered { realisations } if realisations == std::slice::from_ref(&realised))),
            "the realised derivation is registered locally"
        );
    }
}
