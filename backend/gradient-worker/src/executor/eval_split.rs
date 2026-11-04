/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_eval::pool::LiveThunks;
use gradient_sources::ShardSpec;
use gradient_util::sync::Mutex;
use gradient_wire::messages::{FlakeJob, FlakeOutputNode};
use gradient_wire::traits::JobReporter;
use gradient_wire::types::{
    EvalShard, EvalShardOutcome, EvalShardSpec, FinishedShard, JobPhase, SPLIT_COMPANION_ROLE,
    SPLIT_PRIMARY_ROLE,
};
use tracing::{info, warn};

use super::AbortSignal;
use super::eval::{
    CorruptEvalCache, WorkerEvaluator, build_eval_stats_report, build_flake_url, corrupt_cache_in,
    drop_corrupt_cache, eval_cache_fingerprint, eval_cache_path, eval_input_overrides,
    evaluate_shard_with, plan_shards_with, report_failed_attrs,
};
use super::eval_cache_sharing::{EvalCacheSharing, share_eval_cache};
use super::progress_report::report_thunks_during;
use crate::nix::store::LocalNixStore;
use crate::nix::wildcard_walk;
use crate::proto::job::JobUpdater;
use crate::traits::FsDrvReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitMember {
    Primary,
    Companion,
}

impl SplitMember {
    pub fn of_role(role: &str) -> Option<Self> {
        match role {
            SPLIT_PRIMARY_ROLE => Some(Self::Primary),
            SPLIT_COMPANION_ROLE => Some(Self::Companion),
            _ => None,
        }
    }
}

const SHARDS_IN_FLIGHT: usize = 2;

#[async_trait]
pub(crate) trait SplitServer: Send + Sync {
    async fn announce_plan(&self, shards: Vec<EvalShardSpec>) -> Result<()>;

    async fn request(
        &self,
        have_source: bool,
        finished: Option<FinishedShard>,
        held: Vec<String>,
    ) -> Result<EvalShardOutcome>;

    fn shards_in_flight(&self) -> usize;
}

#[async_trait]
pub(crate) trait ShardWork: Send + Sync {
    async fn plan(&mut self) -> Result<Vec<EvalShardSpec>>;

    async fn shard(&self, shard: EvalShard) -> Result<Vec<EvalShardSpec>>;

    async fn flush(&self);

    async fn drop_cache(&mut self, corrupt: &CorruptEvalCache);
}

#[async_trait]
impl SplitServer for JobUpdater {
    async fn announce_plan(&self, shards: Vec<EvalShardSpec>) -> Result<()> {
        self.announce_eval_shards(shards).await
    }

    async fn request(
        &self,
        have_source: bool,
        finished: Option<FinishedShard>,
        held: Vec<String>,
    ) -> Result<EvalShardOutcome> {
        self.request_eval_shard(have_source, finished, held).await
    }

    fn shards_in_flight(&self) -> usize {
        if self.server_keeps_held_eval_shards() {
            SHARDS_IN_FLIGHT
        } else {
            1
        }
    }
}

pub(crate) async fn await_source(server: &dyn SplitServer) -> Result<Option<(String, String)>> {
    loop {
        match server.request(false, None, Vec::new()).await? {
            EvalShardOutcome::Source {
                store_path,
                fingerprint,
            } => return Ok(Some((store_path, fingerprint))),
            EvalShardOutcome::Wait { retry_after_ms } => wait(retry_after_ms).await,
            EvalShardOutcome::Finished => return Ok(None),
            other => bail!("a member without the source cannot be answered with {other:?}"),
        }
    }
}

pub(crate) async fn take_shards(server: &dyn SplitServer, work: &mut dyn ShardWork) -> Result<()> {
    let first = loop {
        let (reply, ()) = tokio::join!(server.request(true, None, Vec::new()), work.flush());
        match reply? {
            EvalShardOutcome::Plan => {
                let planned = work.plan().await;
                let shards = drop_cache_if_corrupt(work, planned).await?;
                work.flush().await;
                server.announce_plan(shards).await?;
            }
            EvalShardOutcome::Wait { retry_after_ms } => wait(retry_after_ms).await,
            reply => break reply,
        }
    };

    let taken = ShardTaker::new(server, &*work).run(first).await;
    drop_cache_if_corrupt(work, taken).await
}

type Listed = (String, Result<Vec<EvalShardSpec>>);

struct ShardTaker<'a> {
    server: &'a dyn SplitServer,
    work: &'a dyn ShardWork,
    slots: usize,
    running: FuturesUnordered<BoxFuture<'a, Listed>>,
    listed: VecDeque<Listed>,
    held: Vec<String>,
}

impl<'a> ShardTaker<'a> {
    fn new(server: &'a dyn SplitServer, work: &'a dyn ShardWork) -> Self {
        Self {
            server,
            work,
            slots: server.shards_in_flight().max(1),
            running: FuturesUnordered::new(),
            listed: VecDeque::new(),
            held: Vec::new(),
        }
    }

    async fn run(mut self, mut reply: EvalShardOutcome) -> Result<()> {
        loop {
            let busy = !self.running.is_empty() || !self.listed.is_empty();
            let room = match reply {
                EvalShardOutcome::Shard(shard) => {
                    self.start(shard);
                    self.running.len() < self.slots
                }
                EvalShardOutcome::Wait { retry_after_ms } if !busy => {
                    wait(retry_after_ms).await;
                    true
                }
                EvalShardOutcome::Wait { retry_after_ms } => {
                    if self.listed.is_empty() {
                        self.wait_or_listed(retry_after_ms).await;
                    }

                    true
                }
                EvalShardOutcome::Finished if !busy => return Ok(()),
                EvalShardOutcome::Finished => false,
                EvalShardOutcome::Plan => {
                    bail!("the plan was asked for after shards were handed out")
                }
                EvalShardOutcome::Source { .. } => {
                    bail!("a member with the source was sent it again")
                }
            };

            if !room && self.listed.is_empty() {
                self.listed.extend(self.running.next().await);
            }
            let finished = self.next_finished()?;
            reply = self.request(finished).await?;
        }
    }

    fn start(&mut self, shard: EvalShard) {
        let (id, work) = (shard.id.clone(), self.work);
        self.held.push(id.clone());
        self.running
            .push(Box::pin(async move { (id, work.shard(shard).await) }));
    }

    async fn wait_or_listed(&mut self, retry_after_ms: u32) {
        tokio::select! {
            () = wait(retry_after_ms) => {}
            Some(listed) = self.running.next(), if !self.running.is_empty() => {
                self.listed.push_back(listed);
            }
        }
    }

    fn next_finished(&mut self) -> Result<Option<FinishedShard>> {
        let Some((id, deferred)) = self.listed.pop_front() else {
            return Ok(None);
        };

        self.held.retain(|held| *held != id);
        Ok(Some(FinishedShard {
            id,
            deferred: deferred?,
        }))
    }

    async fn request(&mut self, finished: Option<FinishedShard>) -> Result<EvalShardOutcome> {
        let (server, work) = (self.server, self.work);
        let held = self.held.clone();
        let request = async move {
            let (reply, ()) = tokio::join!(server.request(true, finished, held), work.flush());
            reply
        };
        tokio::pin!(request);

        loop {
            tokio::select! {
                reply = &mut request => return reply,
                Some(listed) = self.running.next(), if !self.running.is_empty() => {
                    self.listed.push_back(listed);
                }
            }
        }
    }
}

async fn drop_cache_if_corrupt<T>(work: &mut dyn ShardWork, result: Result<T>) -> Result<T> {
    if let Err(e) = &result
        && let Some(corrupt) = corrupt_cache_in(e)
    {
        work.drop_cache(corrupt).await;
    }
    result
}

async fn wait(retry_after_ms: u32) {
    tokio::time::sleep(Duration::from_millis(u64::from(retry_after_ms))).await;
}

pub(super) struct SplitContext<'a> {
    pub(super) evaluator: &'a WorkerEvaluator,
    pub(super) store: &'a LocalNixStore,
    pub(super) job: &'a FlakeJob,
    pub(super) abort: &'a AbortSignal,
}

pub(super) async fn run_primary(
    context: SplitContext<'_>,
    source_path: &str,
    pushed: impl Future<Output = Result<()>>,
    updater: &mut JobUpdater,
) -> Result<()> {
    updater.report_evaluating_derivations().await?;
    let started = Instant::now();
    let thunks = counted_thunks(context.evaluator);
    let sink = updater.eval_progress_sink();
    let server = updater.concurrent();
    let repo = build_flake_url(context.job, Some(source_path));
    let overrides = eval_input_overrides(context.job, Some(source_path));
    let fingerprint = eval_cache_fingerprint(context.evaluator, repo, &overrides).await;
    let announced = fingerprint.clone().unwrap_or_default();
    let announce = async {
        pushed.await?;
        server
            .announce_eval_source(source_path.to_owned(), announced)
            .await
    };
    let mut work = PoolShardWork::open(context, Some(source_path), fingerprint, updater).await;
    let take = report_thunks_during(&*sink, || thunks.total(), take_shards(&server, &mut work));
    tokio::try_join!(announce, take)?;

    work.report_stats(started).await;
    Ok(())
}

pub(super) async fn run_companion(
    context: SplitContext<'_>,
    updater: &mut JobUpdater,
) -> Result<()> {
    let started = Instant::now();
    let thunks = counted_thunks(context.evaluator);
    let sink = updater.eval_progress_sink();
    let server = updater.concurrent();
    let Some((source_path, fingerprint)) = await_source(&server).await? else {
        info!("the split evaluation finished before this companion got its source");
        return Ok(());
    };

    let _phase = updater.phase(JobPhase::EvalDerivations);
    crate::proto::prefetch::ensure_path(context.store, &source_path, updater).await?;
    let fingerprint = (!fingerprint.is_empty()).then_some(fingerprint);
    let mut work = PoolShardWork::open(context, Some(&source_path), fingerprint, updater).await;
    report_thunks_during(&*sink, || thunks.total(), take_shards(&server, &mut work)).await?;

    work.report_stats(started).await;
    Ok(())
}

fn counted_thunks(evaluator: &WorkerEvaluator) -> Arc<LiveThunks> {
    let thunks = evaluator.resolver().live_thunks();
    thunks.reset();
    thunks
}

struct PoolShardWork<'a> {
    context: SplitContext<'a>,
    local_flake_path: Option<&'a str>,
    updater: &'a mut JobUpdater,
    sharing: Option<(EvalCacheSharing, String)>,
    flake_nodes: Mutex<Vec<FlakeOutputNode>>,
}

impl<'a> PoolShardWork<'a> {
    async fn open(
        context: SplitContext<'a>,
        local_flake_path: Option<&'a str>,
        fingerprint: Option<String>,
        updater: &'a mut JobUpdater,
    ) -> Self {
        let sharing = match fingerprint {
            Some(fingerprint) => {
                let path = PathBuf::from(eval_cache_path(context.evaluator, &fingerprint));
                share_eval_cache(path, updater, fingerprint.clone())
                    .await
                    .map(|sharing| (sharing, fingerprint))
            }
            None => None,
        };

        Self {
            context,
            local_flake_path,
            updater,
            sharing,
            flake_nodes: Mutex::default(),
        }
    }

    async fn report_stats(self, started: Instant) {
        let totals = self.context.evaluator.resolver().take_eval_stats();
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let report = build_eval_stats_report(totals, self.flake_nodes.into_inner(), elapsed_ms);
        if let Err(e) = self.updater.report_eval_stats(report).await {
            warn!(error = %e, "failed to send eval stats report");
        }
    }

    async fn pull_drvs(&self, drv_paths: Vec<String>, reporter: &JobUpdater) {
        if drv_paths.is_empty() {
            return;
        }

        let pulled =
            crate::proto::prefetch::pull_cached(self.context.store, drv_paths, reporter).await;
        if let Err(e) = pulled {
            warn!(error = %e, "pulling the derivations of a returned shard failed");
        }
    }
}

#[async_trait]
impl ShardWork for PoolShardWork<'_> {
    async fn plan(&mut self) -> Result<Vec<EvalShardSpec>> {
        let (shards, errors) = plan_shards_with(
            &**self.context.evaluator.resolver(),
            &FsDrvReader,
            self.context.job,
            self.local_flake_path,
            &*self.updater,
            self.context.abort,
        )
        .await?;
        report_failed_attrs(&mut *self.updater, &errors).await?;
        self.updater
            .report_eval_result(Vec::new(), Vec::new(), Vec::new())
            .await?;

        Ok(shards.into_iter().map(shard_spec).collect())
    }

    async fn shard(&self, shard: EvalShard) -> Result<Vec<EvalShardSpec>> {
        let mut reporter = self.updater.concurrent();
        self.pull_drvs(shard.drv_paths, &reporter).await;
        if let Some((sharing, fingerprint)) = &self.sharing {
            sharing.pull(&reporter, fingerprint).await;
        }

        let given = ShardSpec {
            pattern: shard.pattern,
            only: shard.children,
        };
        let outcome = evaluate_shard_with(
            &**self.context.evaluator.resolver(),
            &FsDrvReader,
            self.context.job,
            self.local_flake_path,
            &mut reporter,
            self.context.abort,
            vec![given],
        )
        .await?;
        self.flake_nodes.lock().extend(outcome.flake_nodes);

        Ok(outcome.deferred.into_iter().map(shard_spec).collect())
    }

    async fn flush(&self) {
        if let Some((sharing, _)) = &self.sharing {
            sharing.flush().await;
        }
    }

    async fn drop_cache(&mut self, corrupt: &CorruptEvalCache) {
        self.sharing = None;
        let resolver = self.context.evaluator.resolver();
        drop_corrupt_cache(&**resolver, resolver.eval_cache_dir(), corrupt).await;
    }
}

fn shard_spec(shard: ShardSpec) -> EvalShardSpec {
    EvalShardSpec {
        set: set_of(&shard.pattern),
        pattern: shard.pattern,
        children: shard.only,
    }
}

fn set_of(pattern: &str) -> Vec<String> {
    wildcard_walk::parse_pattern(pattern)
        .1
        .into_iter()
        .take_while(|segment| !segment.contains('*'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::traits::EvalProgressSink;
    use gradient_wire::types::EvalProgress;
    use std::sync::atomic::{AtomicU64, Ordering};

    type Request = (bool, Option<FinishedShard>, Vec<String>);

    #[derive(Default)]
    struct ScriptedServer {
        replies: Mutex<VecDeque<EvalShardOutcome>>,
        requests: Mutex<Vec<Request>>,
        plans: Mutex<Vec<Vec<EvalShardSpec>>>,
        events: Arc<Mutex<Vec<&'static str>>>,
        slots: usize,
    }

    impl ScriptedServer {
        fn new(replies: Vec<EvalShardOutcome>, events: &Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                events: Arc::clone(events),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl SplitServer for ScriptedServer {
        async fn announce_plan(&self, shards: Vec<EvalShardSpec>) -> Result<()> {
            self.plans.lock().push(shards);
            Ok(())
        }

        async fn request(
            &self,
            have_source: bool,
            finished: Option<FinishedShard>,
            held: Vec<String>,
        ) -> Result<EvalShardOutcome> {
            self.requests.lock().push((have_source, finished, held));
            self.events.lock().push("request");
            let reply = self.replies.lock().pop_front();
            reply.ok_or_else(|| anyhow::anyhow!("no reply scripted"))
        }

        fn shards_in_flight(&self) -> usize {
            self.slots
        }
    }

    struct ScriptedWork {
        deferred: Mutex<Vec<EvalShardSpec>>,
        corrupt: bool,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl ShardWork for ScriptedWork {
        async fn plan(&mut self) -> Result<Vec<EvalShardSpec>> {
            self.events.lock().push("plan");
            Ok(Vec::new())
        }

        async fn shard(&self, _shard: EvalShard) -> Result<Vec<EvalShardSpec>> {
            self.events.lock().push("shard");
            if self.corrupt {
                let corrupt = CorruptEvalCache {
                    fingerprint: "ab".into(),
                };
                return Err(anyhow::Error::new(corrupt).context("listing the shard"));
            }
            Ok(std::mem::take(&mut *self.deferred.lock()))
        }

        async fn flush(&self) {
            self.events.lock().push("flush");
        }

        async fn drop_cache(&mut self, _corrupt: &CorruptEvalCache) {
            self.events.lock().push("drop cache");
        }
    }

    fn shard(id: &str) -> EvalShardOutcome {
        EvalShardOutcome::Shard(EvalShard {
            id: id.into(),
            pattern: "legacyPackages.x86_64-linux.*".into(),
            children: Some(vec!["python3Packages".into()]),
            drv_paths: Vec::new(),
        })
    }

    fn python_set() -> EvalShardSpec {
        EvalShardSpec {
            pattern: "legacyPackages.x86_64-linux.python3Packages.*".into(),
            set: vec![
                "legacyPackages".into(),
                "x86_64-linux".into(),
                "python3Packages".into(),
            ],
            children: Some(vec!["numpy".into()]),
        }
    }

    #[tokio::test]
    async fn a_companion_waits_for_the_source_before_evaluating() {
        let events = Arc::default();
        let server = ScriptedServer::new(
            vec![
                EvalShardOutcome::Wait { retry_after_ms: 0 },
                EvalShardOutcome::Source {
                    store_path: "/nix/store/[hash150]-source".into(),
                    fingerprint: "fp".into(),
                },
            ],
            &events,
        );

        let source = await_source(&server).await.expect("source");

        assert_eq!(
            source,
            Some(("/nix/store/[hash150]-source".into(), "fp".into()))
        );
        let requests = server.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|(have_source, finished, held)| !have_source
                    && finished.is_none()
                    && held.is_empty())
        );
    }

    #[tokio::test]
    async fn the_taker_reports_deferred_sets_with_the_next_request() {
        let events = Arc::default();
        let server = ScriptedServer::new(vec![shard("s1"), EvalShardOutcome::Finished], &events);
        let mut work = ScriptedWork {
            deferred: Mutex::new(vec![python_set()]),
            corrupt: false,
            events: Arc::clone(&events),
        };

        take_shards(&server, &mut work).await.expect("taken");

        let requests = server.requests.lock();
        assert_eq!(requests[0], (true, None, Vec::new()));
        assert_eq!(
            requests[1],
            (
                true,
                Some(FinishedShard {
                    id: "s1".into(),
                    deferred: vec![python_set()],
                }),
                Vec::new()
            )
        );
    }

    #[tokio::test]
    async fn finished_ends_the_job_after_the_last_flush() {
        let events = Arc::default();
        let server = ScriptedServer::new(
            vec![
                EvalShardOutcome::Plan,
                shard("s1"),
                EvalShardOutcome::Finished,
            ],
            &events,
        );
        let mut work = ScriptedWork {
            deferred: Mutex::default(),
            corrupt: false,
            events: Arc::clone(&events),
        };

        take_shards(&server, &mut work).await.expect("taken");

        let events = events.lock().clone();
        let last_shard = events.iter().rposition(|e| *e == "shard").expect("a shard");
        assert!(
            events[last_shard..].contains(&"flush"),
            "the shard is flushed before the job ends: {events:?}"
        );
        assert_eq!(server.plans.lock().len(), 1);
        assert_eq!(events.first(), Some(&"request"));
    }

    #[tokio::test]
    async fn a_corrupt_cache_fails_the_member_not_the_evaluation() {
        let events = Arc::default();
        let server = ScriptedServer::new(vec![shard("s1"), shard("s2")], &events);
        let mut work = ScriptedWork {
            deferred: Mutex::default(),
            corrupt: true,
            events: Arc::clone(&events),
        };

        let error = take_shards(&server, &mut work)
            .await
            .expect_err("the member fails");

        assert!(corrupt_cache_in(&error).is_some(), "{error:#}");
        assert_eq!(*server.requests.lock(), vec![(true, None, Vec::new())]);
        let events = events.lock().clone();
        let listed = events.iter().position(|e| *e == "shard").expect("a shard");
        assert_eq!(events[listed..], ["shard", "drop cache"]);
    }

    struct SlowWork {
        thunks: Arc<AtomicU64>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl ShardWork for SlowWork {
        async fn plan(&mut self) -> Result<Vec<EvalShardSpec>> {
            Ok(Vec::new())
        }

        async fn shard(&self, _shard: EvalShard) -> Result<Vec<EvalShardSpec>> {
            self.events.lock().push("shard");
            self.thunks.store(40, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(3)).await;
            self.events.lock().push("shard listed");
            Ok(Vec::new())
        }

        async fn flush(&self) {}

        async fn drop_cache(&mut self, _corrupt: &CorruptEvalCache) {}
    }

    struct TimedWork;

    #[async_trait]
    impl ShardWork for TimedWork {
        async fn plan(&mut self) -> Result<Vec<EvalShardSpec>> {
            Ok(Vec::new())
        }

        async fn shard(&self, shard: EvalShard) -> Result<Vec<EvalShardSpec>> {
            let secs = if shard.id == "slow" { 3 } else { 1 };
            tokio::time::sleep(Duration::from_secs(secs)).await;
            Ok(Vec::new())
        }

        async fn flush(&self) {}

        async fn drop_cache(&mut self, _corrupt: &CorruptEvalCache) {}
    }

    fn done(id: &str) -> Option<FinishedShard> {
        Some(FinishedShard {
            id: id.into(),
            deferred: Vec::new(),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn the_next_shard_is_taken_while_a_slow_one_is_still_listed() {
        let events = Arc::default();
        let server = ScriptedServer {
            slots: 2,
            ..ScriptedServer::new(
                vec![
                    shard("slow"),
                    shard("fast"),
                    EvalShardOutcome::Wait { retry_after_ms: 0 },
                    EvalShardOutcome::Finished,
                    EvalShardOutcome::Finished,
                ],
                &events,
            )
        };
        let started = tokio::time::Instant::now();

        take_shards(&server, &mut TimedWork).await.expect("taken");

        assert_eq!(started.elapsed(), Duration::from_secs(3));
        assert_eq!(
            *server.requests.lock(),
            vec![
                (true, None, Vec::new()),
                (true, None, vec!["slow".to_owned()]),
                (true, done("fast"), vec!["slow".to_owned()]),
                (true, None, vec!["slow".to_owned()]),
                (true, done("slow"), Vec::new()),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_while_a_shard_runs_is_asked_again_after_the_retry_delay() {
        let events = Arc::default();
        let server = ScriptedServer {
            slots: 2,
            ..ScriptedServer::new(
                vec![
                    shard("slow"),
                    EvalShardOutcome::Wait {
                        retry_after_ms: 1000,
                    },
                    shard("fast"),
                    EvalShardOutcome::Finished,
                    EvalShardOutcome::Finished,
                ],
                &events,
            )
        };
        let started = tokio::time::Instant::now();

        take_shards(&server, &mut TimedWork).await.expect("taken");

        assert_eq!(started.elapsed(), Duration::from_secs(3));
        assert_eq!(
            *server.requests.lock(),
            vec![
                (true, None, Vec::new()),
                (true, None, vec!["slow".to_owned()]),
                (true, None, vec!["slow".to_owned()]),
                (true, done("fast"), vec!["slow".to_owned()]),
                (true, done("slow"), Vec::new()),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_older_server_gets_one_shard_at_a_time() {
        let events = Arc::default();
        let server = ScriptedServer::new(
            vec![shard("slow"), shard("fast"), EvalShardOutcome::Finished],
            &events,
        );
        let started = tokio::time::Instant::now();

        take_shards(&server, &mut TimedWork).await.expect("taken");

        assert_eq!(started.elapsed(), Duration::from_secs(4));
        assert!(
            server
                .requests
                .lock()
                .iter()
                .all(|(_, _, held)| held.is_empty())
        );
    }

    struct EventSink(Arc<Mutex<Vec<&'static str>>>);

    #[async_trait]
    impl EvalProgressSink for EventSink {
        async fn report(&self, progress: EvalProgress) {
            assert_eq!(progress, EvalProgress::Evaluating { thunks: 40 });
            self.0.lock().push("progress");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_taker_reports_thunks_while_a_shard_is_listed() {
        let events = Arc::default();
        let server = ScriptedServer::new(vec![shard("s1"), EvalShardOutcome::Finished], &events);
        let thunks = Arc::new(AtomicU64::new(0));
        let mut work = SlowWork {
            thunks: Arc::clone(&thunks),
            events: Arc::clone(&events),
        };
        let sink = EventSink(Arc::clone(&events));

        report_thunks_during(
            &sink,
            || thunks.load(Ordering::Relaxed),
            take_shards(&server, &mut work),
        )
        .await
        .expect("taken");

        let events = events.lock().clone();
        let listing = events.iter().position(|e| *e == "shard").expect("a shard");
        let listed = events
            .iter()
            .position(|e| *e == "shard listed")
            .expect("listed");
        assert!(
            events[listing..listed].contains(&"progress"),
            "progress is sent while the shard is listed: {events:?}"
        );
    }

    #[test]
    fn a_split_set_is_the_path_before_the_first_wildcard() {
        assert_eq!(
            set_of("legacyPackages.*-linux.hello"),
            vec!["legacyPackages".to_owned()]
        );
        assert_eq!(
            SplitMember::of_role(SPLIT_PRIMARY_ROLE),
            Some(SplitMember::Primary)
        );
        assert_eq!(SplitMember::of_role("server"), None);
    }
}
