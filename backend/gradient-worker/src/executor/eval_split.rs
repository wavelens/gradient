/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use async_trait::async_trait;
use gradient_eval::pool::LiveThunks;
use gradient_sources::ShardSpec;
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

#[async_trait]
pub(crate) trait SplitServer: Send + Sync {
    async fn announce_plan(&self, shards: Vec<EvalShardSpec>) -> Result<()>;

    async fn request(
        &self,
        have_source: bool,
        finished: Option<FinishedShard>,
    ) -> Result<EvalShardOutcome>;
}

#[async_trait]
pub(crate) trait ShardWork: Send {
    async fn plan(&mut self) -> Result<Vec<EvalShardSpec>>;

    async fn shard(&mut self, shard: EvalShard) -> Result<Vec<EvalShardSpec>>;

    async fn flush(&mut self);

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
    ) -> Result<EvalShardOutcome> {
        self.request_eval_shard(have_source, finished).await
    }
}

pub(crate) async fn await_source(server: &dyn SplitServer) -> Result<Option<(String, String)>> {
    loop {
        match server.request(false, None).await? {
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
    let mut finished = None;
    loop {
        let (reply, ()) = tokio::join!(server.request(true, finished.take()), work.flush());
        match reply? {
            EvalShardOutcome::Plan => {
                let planned = work.plan().await;
                let shards = drop_cache_if_corrupt(work, planned).await?;
                work.flush().await;
                server.announce_plan(shards).await?;
            }
            EvalShardOutcome::Shard(shard) => {
                let id = shard.id.clone();
                let listed = work.shard(shard).await;
                let deferred = drop_cache_if_corrupt(work, listed).await?;
                finished = Some(FinishedShard { id, deferred });
            }
            EvalShardOutcome::Wait { retry_after_ms } => wait(retry_after_ms).await,
            EvalShardOutcome::Finished => return Ok(()),
            EvalShardOutcome::Source { .. } => bail!("a member with the source was sent it again"),
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
    flake_nodes: Vec<FlakeOutputNode>,
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
            flake_nodes: Vec::new(),
        }
    }

    async fn report_stats(self, started: Instant) {
        let totals = self.context.evaluator.resolver().take_eval_stats();
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let report = build_eval_stats_report(totals, self.flake_nodes, elapsed_ms);
        if let Err(e) = self.updater.report_eval_stats(report).await {
            warn!(error = %e, "failed to send eval stats report");
        }
    }

    async fn pull_drvs(&mut self, drv_paths: Vec<String>) {
        if drv_paths.is_empty() {
            return;
        }

        let pulled =
            crate::proto::prefetch::pull_cached(self.context.store, drv_paths, &*self.updater)
                .await;
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

    async fn shard(&mut self, shard: EvalShard) -> Result<Vec<EvalShardSpec>> {
        self.pull_drvs(shard.drv_paths).await;
        if let Some((sharing, fingerprint)) = &self.sharing {
            sharing.pull(&*self.updater, fingerprint).await;
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
            &mut *self.updater,
            self.context.abort,
            vec![given],
        )
        .await?;
        self.flake_nodes.extend(outcome.flake_nodes);

        Ok(outcome.deferred.into_iter().map(shard_spec).collect())
    }

    async fn flush(&mut self) {
        if let Some((sharing, _)) = &mut self.sharing {
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
    use gradient_util::sync::Mutex;
    use gradient_wire::traits::EvalProgressSink;
    use gradient_wire::types::EvalProgress;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct ScriptedServer {
        replies: Mutex<VecDeque<EvalShardOutcome>>,
        requests: Mutex<Vec<(bool, Option<FinishedShard>)>>,
        plans: Mutex<Vec<Vec<EvalShardSpec>>>,
        events: Arc<Mutex<Vec<&'static str>>>,
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
        ) -> Result<EvalShardOutcome> {
            self.requests.lock().push((have_source, finished));
            self.events.lock().push("request");
            let reply = self.replies.lock().pop_front();
            reply.ok_or_else(|| anyhow::anyhow!("no reply scripted"))
        }
    }

    struct ScriptedWork {
        deferred: Vec<EvalShardSpec>,
        corrupt: bool,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl ShardWork for ScriptedWork {
        async fn plan(&mut self) -> Result<Vec<EvalShardSpec>> {
            self.events.lock().push("plan");
            Ok(Vec::new())
        }

        async fn shard(&mut self, _shard: EvalShard) -> Result<Vec<EvalShardSpec>> {
            self.events.lock().push("shard");
            if self.corrupt {
                let corrupt = CorruptEvalCache {
                    fingerprint: "ab".into(),
                };
                return Err(anyhow::Error::new(corrupt).context("listing the shard"));
            }
            Ok(std::mem::take(&mut self.deferred))
        }

        async fn flush(&mut self) {
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
                .all(|(have_source, finished)| !have_source && finished.is_none())
        );
    }

    #[tokio::test]
    async fn the_taker_reports_deferred_sets_with_the_next_request() {
        let events = Arc::default();
        let server = ScriptedServer::new(vec![shard("s1"), EvalShardOutcome::Finished], &events);
        let mut work = ScriptedWork {
            deferred: vec![python_set()],
            corrupt: false,
            events: Arc::clone(&events),
        };

        take_shards(&server, &mut work).await.expect("taken");

        let requests = server.requests.lock();
        assert_eq!(requests[0], (true, None));
        assert_eq!(
            requests[1],
            (
                true,
                Some(FinishedShard {
                    id: "s1".into(),
                    deferred: vec![python_set()],
                })
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
            deferred: Vec::new(),
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
            deferred: Vec::new(),
            corrupt: true,
            events: Arc::clone(&events),
        };

        let error = take_shards(&server, &mut work)
            .await
            .expect_err("the member fails");

        assert!(corrupt_cache_in(&error).is_some(), "{error:#}");
        assert_eq!(*server.requests.lock(), vec![(true, None)]);
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

        async fn shard(&mut self, _shard: EvalShard) -> Result<Vec<EvalShardSpec>> {
            self.events.lock().push("shard");
            self.thunks.store(40, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(3)).await;
            self.events.lock().push("shard listed");
            Ok(Vec::new())
        }

        async fn flush(&mut self) {}

        async fn drop_cache(&mut self, _corrupt: &CorruptEvalCache) {}
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
