/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The graph module: one actor owns every write to the dependency graph and
//! the cache index. [`Graph`] is the handle the rest of the server calls.

pub mod actor;
pub mod messages;

mod demote;
mod ingest;
mod known;
mod nar;
pub mod policy;
mod requeue;
mod self_heal;
mod transition;

use std::sync::Arc;

use gradient_db::DbContext;
use gradient_util::supervision::{ChildCtx, ChildSpec, SupervisorHealth};
use ractor::rpc::CallResult;
use ractor::{Actor, ActorCell, ActorRef, RpcReplyPort, SpawnErr};
use tokio::sync::watch;

use actor::{CALL_TIMEOUT, GraphActor, GraphArgs, GraphMsg, HEALTH_NAME, RPC_TIMEOUT};
pub use messages::*;
pub use policy::retry_backoff_elapsed;

/// The live actor, republished on every (re)spawn; a caller waits on the
/// watch so a restart looks like latency.
pub struct Graph {
    actor: watch::Sender<Option<ActorRef<GraphMsg>>>,
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
            actor: watch::channel(None).0,
            #[cfg(feature = "stub")]
            stub: false,
        })
    }

    /// A handle that answers every call itself, reaching no actor and no
    /// database. For harnesses whose subject is a caller of the graph.
    #[cfg(feature = "stub")]
    pub fn stub() -> Arc<Self> {
        Arc::new(Self {
            actor: watch::channel(None).0,
            stub: true,
        })
    }

    /// The root child that runs the actor; stopped after every sibling.
    pub fn child_spec(self: &Arc<Self>, ctx: DbContext) -> ChildSpec {
        let graph = Arc::clone(self);
        ChildSpec::Custom {
            name: HEALTH_NAME,
            stop_last: true,
            spawn: Arc::new(move |child: ChildCtx| {
                let graph = Arc::clone(&graph);
                let ctx = ctx.clone();
                Box::pin(async move {
                    let actor = graph
                        .spawn(ctx, Some(child.health), Some(child.parent))
                        .await?;
                    Ok(actor.get_cell())
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
        let args = GraphArgs { ctx, health };
        let (actor, _) = match parent {
            Some(parent) => Actor::spawn_linked(None, GraphActor, args, parent).await?,
            None => Actor::spawn(None, GraphActor, args).await?,
        };
        self.actor.send_replace(Some(actor.clone()));
        Ok(actor)
    }

    async fn live(&self) -> anyhow::Result<ActorRef<GraphMsg>> {
        let mut rx = self.actor.subscribe();
        let live = tokio::time::timeout(CALL_TIMEOUT, rx.wait_for(|a| a.is_some()))
            .await
            .map_err(|_| anyhow::anyhow!("graph actor unavailable"))?
            .map_err(|_| anyhow::anyhow!("graph actor closed"))?;
        Ok(live.clone().expect("wait_for guarantees Some"))
    }

    async fn call<T: Send + 'static>(
        &self,
        msg: impl FnOnce(RpcReplyPort<anyhow::Result<T>>) -> GraphMsg,
    ) -> anyhow::Result<T> {
        match self.live().await?.call(msg, Some(RPC_TIMEOUT)).await {
            Ok(CallResult::Success(result)) => result,
            Ok(CallResult::Timeout) => Err(anyhow::anyhow!("graph call timed out")),
            Ok(CallResult::SenderError) => Err(anyhow::anyhow!("graph actor dropped the reply")),
            Err(e) => Err(anyhow::anyhow!("graph actor unreachable: {e}")),
        }
    }

    pub async fn ingest(&self, batch: IngestBatch) -> anyhow::Result<IngestReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(IngestReport::default());
        }
        self.call(|reply| GraphMsg::Ingest(batch, reply)).await
    }

    /// Store paths of `drv_hashes` the worker may prune, answered after every
    /// write queued before this call.
    pub async fn known_derivations(&self, drv_hashes: Vec<String>) -> anyhow::Result<Vec<String>> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(Vec::new());
        }
        self.call(|reply| GraphMsg::KnownDerivations { drv_hashes, reply })
            .await
    }

    /// Record a NAR already in storage: the `cached_path` row, its references,
    /// signature placeholders and the outputs it backs, in one transaction.
    pub async fn commit_nar(&self, commit: NarCommit) -> anyhow::Result<NarCommitted> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(NarCommitted {
                cached_path: gradient_types::ids::CachedPathId::now_v7(),
                created: true,
                outputs_marked: 0,
            });
        }
        self.call(|reply| GraphMsg::CommitNar(commit, reply)).await
    }

    pub async fn transition(&self, transition: Transition) -> anyhow::Result<TransitionReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(TransitionReport::default());
        }
        self.call(|reply| GraphMsg::Transition(transition, reply))
            .await
    }

    /// Move anchors back to `Queued`; returns how many moved.
    pub async fn requeue(&self, scope: RequeueScope) -> anyhow::Result<u64> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(0);
        }
        self.call(|reply| GraphMsg::Requeue(scope, reply)).await
    }

    /// Drop a path's claim on the cache index, or a whole sweep of them.
    pub async fn demote(&self, demotion: Demotion) -> anyhow::Result<DemoteReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(DemoteReport::default());
        }
        self.call(|reply| GraphMsg::Demote(demotion, reply)).await
    }
}

#[cfg(test)]
pub(crate) mod test_ctx {
    use std::sync::Arc;

    use clap::Parser as _;
    use gradient_db::{DbContext, NoReactor, WebDb, WorkerDb};
    use gradient_storage::{FileLogStorage, NarStore, StorageCtx};
    use gradient_types::{Cli, RuntimeConfig};
    use gradient_util::shutdown::Shutdown;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

    /// A context over `db`, plus the pool handle its transaction log is read from.
    pub(crate) async fn ctx(db: DatabaseConnection) -> (DbContext, WorkerDb) {
        let dir = std::env::temp_dir().join(format!("gradient-graph-{}", uuid::Uuid::now_v7()));
        let cli = Cli::try_parse_from([
            "gradient-server",
            "--crypt-secret-file",
            "test-secret",
            "--jwt-secret-file",
            "test-jwt",
            "--serve-url",
            "http://127.0.0.1:3000",
            "--base-path",
            dir.to_str().unwrap(),
        ])
        .expect("test cli");
        let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("test config"));
        let worker_db = WorkerDb::new(db);
        let ctx = DbContext {
            worker_db: worker_db.clone(),
            web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            config,
            storage: StorageCtx {
                nar_storage: NarStore::local(dir.to_str().unwrap()).unwrap(),
                log_storage: Arc::new(FileLogStorage::new(&dir).await.unwrap()),
            },
            shutdown: Shutdown::new(),
            board_events: tokio::sync::broadcast::channel(16).0,
            reactor: Arc::new(NoReactor),
        };
        (ctx, worker_db)
    }
}
