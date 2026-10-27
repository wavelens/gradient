/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![recursion_limit = "256"]

pub mod messages;
pub mod writer;

#[cfg(test)]
mod cases;
mod claims;
mod demote;
mod exec;
mod gc;
mod known;
mod nar;
pub mod policy;
mod record;
mod requeue;
mod self_heal;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod transition;

use std::sync::{Arc, OnceLock};

use gradient_db::DbContext;
use gradient_types::DerivationId;
use gradient_types::events::{Event, EventBus, graph};
use gradient_util::supervision::{ChildCtx, ChildSpec, SupervisorHealth};
use ractor::rpc::CallResult;
use ractor::{Actor, ActorCell, ActorRef, RpcReplyPort, SpawnErr};
use tokio::sync::watch;

pub use messages::*;
pub use policy::retry_backoff_elapsed;
use writer::{CALL_TIMEOUT, GraphArgs, GraphMsg, GraphWriter, HEALTH_NAME};

pub struct Graph {
    writer: watch::Sender<Option<ActorRef<GraphMsg>>>,
    events: OnceLock<EventBus>,
    reads: OnceLock<gradient_db::WorkerDb>,
    #[cfg(feature = "stub")]
    stub: bool,
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph").finish_non_exhaustive()
    }
}

impl Graph {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            writer: watch::channel(None).0,
            events: OnceLock::new(),
            reads: OnceLock::new(),
            #[cfg(feature = "stub")]
            stub: false,
        })
    }

    #[cfg(feature = "stub")]
    pub fn stub() -> Arc<Self> {
        Arc::new(Self {
            writer: watch::channel(None).0,
            events: OnceLock::new(),
            reads: OnceLock::new(),
            stub: true,
        })
    }

    pub fn child_spec(self: &Arc<Self>, ctx: DbContext) -> ChildSpec {
        let graph = Arc::clone(self);
        ChildSpec::Custom {
            name: HEALTH_NAME,
            stop_last: true,
            spawn: Arc::new(move |child: ChildCtx| {
                let graph = Arc::clone(&graph);
                let ctx = ctx.clone();
                Box::pin(async move {
                    let writer = graph
                        .spawn(ctx, Some(child.health), Some(child.parent))
                        .await?;
                    Ok(writer.get_cell())
                })
            }),
        }
    }

    pub async fn spawn(
        &self,
        ctx: DbContext,
        health: Option<Arc<SupervisorHealth>>,
        parent: Option<ActorCell>,
    ) -> Result<ActorRef<GraphMsg>, SpawnErr> {
        let _ = self.events.set(ctx.events.clone());
        let _ = self.reads.set(ctx.worker_db.clone());
        let args = GraphArgs { ctx, health };
        let (writer, _) = match parent {
            Some(parent) => Actor::spawn_linked(None, GraphWriter, args, parent).await?,
            None => Actor::spawn(None, GraphWriter, args).await?,
        };
        self.writer.send_replace(Some(writer.clone()));
        Ok(writer)
    }

    fn announce<E: Into<Event>>(&self, event: impl FnOnce() -> E) {
        if let Some(bus) = self.events.get() {
            bus.publish(event());
        }
    }

    async fn live(&self) -> anyhow::Result<ActorRef<GraphMsg>> {
        let mut rx = self.writer.subscribe();
        let live = tokio::time::timeout(CALL_TIMEOUT, rx.wait_for(|a| a.is_some()))
            .await
            .map_err(|_| anyhow::anyhow!("graph writer unavailable"))?
            .map_err(|_| anyhow::anyhow!("graph writer closed"))?;
        Ok(live.clone().expect("wait_for guarantees Some"))
    }

    async fn call<T: Send + 'static>(
        &self,
        msg: impl FnOnce(RpcReplyPort<anyhow::Result<T>>) -> GraphMsg,
    ) -> anyhow::Result<T> {
        match self.live().await?.call(msg, None).await {
            Ok(CallResult::Success(result)) => result,
            Ok(CallResult::SenderError | CallResult::Timeout) => {
                Err(anyhow::anyhow!("graph writer dropped the reply"))
            }
            Err(e) => Err(anyhow::anyhow!("graph writer unreachable: {e}")),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(eval_id = %batch.evaluation, derivations = batch.derivations.len()))]
    pub async fn record(&self, batch: RecordBatch) -> anyhow::Result<RecordReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(RecordReport::default());
        }
        let report = self.call(|reply| GraphMsg::Record(batch, reply)).await?;
        self.announce(|| graph::Recorded {
            evaluation_id: report.evaluation,
            task: report.task,
            walked: report.walked,
            entry_points: report.entry_points.len(),
            skipped: report.skipped,
        });
        Ok(report)
    }

    /// The read is going to the pool, not behind the graph writer.
    /// A committed subtree can only ever gain its record.
    /// A read that misses a queued write is pruning less, never wrongly.
    #[tracing::instrument(level = "debug", skip_all, fields(paths = drv_hashes.len()))]
    pub async fn known_derivations(&self, drv_hashes: Vec<String>) -> anyhow::Result<Vec<String>> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(Vec::new());
        }
        let db = self
            .reads
            .get()
            .ok_or_else(|| anyhow::anyhow!("graph writer never started"))?;
        Ok(known::prunable(db, drv_hashes).await?)
    }

    pub async fn upstream_hits(
        &self,
        hits: std::collections::HashMap<String, UpstreamHit>,
    ) -> anyhow::Result<()> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(());
        }
        self.call(|reply| GraphMsg::UpstreamHits(hits, reply)).await
    }

    pub async fn upstream_probed(&self, shared_builds: Vec<DerivationId>) -> anyhow::Result<()> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(());
        }
        self.call(|reply| GraphMsg::UpstreamProbed(shared_builds, reply))
            .await
    }

    pub async fn commit_nar(&self, commit: NarCommit) -> anyhow::Result<NarCommitted> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(NarCommitted {
                cached_path: gradient_types::ids::CachedPathId::now_v7(),
                created: true,
                signed: Vec::new(),
            });
        }
        let committed = self
            .call(|reply| GraphMsg::CommitNar(commit, reply))
            .await?;
        self.announce(|| graph::NarCommitted {
            cached_path: committed.cached_path,
            created: committed.created,
        });
        Ok(committed)
    }

    pub async fn transition(&self, transition: Transition) -> anyhow::Result<TransitionReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(TransitionReport::default());
        }
        let report = self
            .call(|reply| GraphMsg::Transition(transition, reply))
            .await?;
        self.announce(|| graph::Transitioned {
            aborted: report.aborted_shared_builds.len(),
            prioritized: report.prioritized_shared_builds.len(),
        });
        Ok(report)
    }

    pub async fn requeue(&self, scope: RequeueScope) -> anyhow::Result<u64> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(0);
        }
        let requeued = self.call(|reply| GraphMsg::Requeue(scope, reply)).await?;
        self.announce(|| graph::Requeued { requeued });
        Ok(requeued)
    }

    pub async fn demote(&self, demotion: Demotion) -> anyhow::Result<DemoteReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(DemoteReport::default());
        }
        let report = self.call(|reply| GraphMsg::Demote(demotion, reply)).await?;
        self.announce(|| graph::Demoted {
            producers: report.producers.len(),
            others_remain: report.others_remain,
        });
        Ok(report)
    }

    pub async fn gc(&self, request: GcRequest) -> anyhow::Result<GcReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(GcReport::default());
        }
        let report = self.call(|reply| GraphMsg::Gc(request, reply)).await?;
        self.announce(|| graph::Collected {
            derivations: report.deleted_derivations.len(),
            evaluations: report.deleted_evaluations.len(),
            retired_paths: report.retired.len(),
        });
        Ok(report)
    }
}

/// The linker is dropping an rlib nothing mentions, including its `gradient_db::sql!` entries.
/// This call is keeping the crate's statements in the plan gate's registry.
pub const fn link() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_graph_without_a_bus_reports_nothing_and_does_not_panic() {
        let graph = Graph::new();
        graph.announce(|| graph::Requeued { requeued: 1 });
    }

    #[tokio::test]
    async fn a_graph_with_a_bus_announces_on_it() {
        let graph = Graph::new();
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();
        graph.events.set(bus).unwrap();

        graph.announce(|| graph::Requeued { requeued: 1 });

        assert_eq!(rx.try_recv().unwrap().event.name(), "graph.requeued");
    }

    struct Backlogged;

    impl Actor for Backlogged {
        type Msg = GraphMsg;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<GraphMsg>,
            _args: (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<GraphMsg>,
            msg: GraphMsg,
            _state: &mut (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            if let GraphMsg::UpstreamProbed(_, reply) = msg {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                let _ = reply.send(Ok(()));
            }
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_behind_a_backlog_gets_the_answer_the_actor_still_gives() {
        let graph = Graph::new();
        let (writer, _) = Actor::spawn(None, Backlogged, ()).await.unwrap();
        graph.writer.send_replace(Some(writer.clone()));

        graph
            .upstream_probed(vec![gradient_types::DerivationId::now_v7()])
            .await
            .unwrap();

        writer.stop(None);
    }
}

#[cfg(test)]
pub(crate) mod test_ctx {
    use gradient_db::{DbContext, WorkerDb};

    pub(crate) use crate::messages::{RecordBatch as Batch, RecordReport as BatchReport};
    pub(crate) use crate::testing::{ctx_with_crypt_file, secret_file, signing_key};

    pub(crate) async fn send(graph: &crate::Graph, batch: Batch) -> anyhow::Result<BatchReport> {
        graph.record(batch).await
    }

    pub(crate) async fn apply_in(scoped: &DbContext, batch: &Batch) -> anyhow::Result<BatchReport> {
        crate::record::apply_batch(scoped, batch).await
    }

    pub(crate) async fn claim_build(
        db: &WorkerDb,
        assignment: gradient_entity::dispatched_job::Model,
        shared_build: gradient_types::ids::DerivationBuildId,
        substitute: bool,
    ) -> bool {
        use gradient_db::scheduling::assignment_record::{ClaimGate, claim_assignment};

        let build = ClaimGate::Build {
            shared_build,
            substitute,
        };
        claim_assignment(db, assignment, build)
            .await
            .expect("the claim runs")
    }

    pub(crate) fn worker_loss(
        shared_builds: Vec<gradient_types::ids::DerivationBuildId>,
        cause: crate::OrphanCause,
    ) -> crate::Transition {
        crate::Transition::OrphanedBuilds {
            shared_builds,
            cause,
            observed_peak_ram_mb: Default::default(),
        }
    }
}
