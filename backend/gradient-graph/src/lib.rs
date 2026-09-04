/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The graph module: one actor owns every write to the dependency graph and
//! the cache index. [`Graph`] is the handle the rest of the server calls.

pub mod actor;
pub mod messages;

mod ingest;
mod known;
mod transition;

use std::sync::Arc;

use gradient_db::DbContext;
use gradient_util::supervision::{ChildCtx, ChildSpec, SupervisorHealth};
use ractor::rpc::CallResult;
use ractor::{Actor, ActorCell, ActorRef, RpcReplyPort, SpawnErr};
use tokio::sync::watch;

use actor::{CALL_TIMEOUT, GraphActor, GraphArgs, GraphMsg, HEALTH_NAME, RPC_TIMEOUT};
pub use messages::*;

/// The live actor, republished on every (re)spawn; a caller waits on the
/// watch so a restart looks like latency.
pub struct Graph {
    actor: watch::Sender<Option<ActorRef<GraphMsg>>>,
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
        self.call(|reply| GraphMsg::Ingest(batch, reply)).await
    }

    /// Store paths of `drv_hashes` the worker may prune, answered after every
    /// write queued before this call.
    pub async fn known_derivations(&self, drv_hashes: Vec<String>) -> anyhow::Result<Vec<String>> {
        self.call(|reply| GraphMsg::KnownDerivations { drv_hashes, reply })
            .await
    }

    pub async fn transition(&self, transition: Transition) -> anyhow::Result<TransitionReport> {
        self.call(|reply| GraphMsg::Transition(transition, reply))
            .await
    }
}
