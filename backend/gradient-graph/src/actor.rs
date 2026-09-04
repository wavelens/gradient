/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one writer of the dependency graph and the cache index. Every message is
//! one transaction; ingest batches queued together are one transaction with a
//! savepoint per batch, and any other message flushes that queue first, which is
//! what makes a known-derivations query read its callers' earlier writes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use gradient_db::DbContext;
use gradient_types::ids::EvaluationId;
use gradient_util::supervision::SupervisorHealth;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use sea_orm::TransactionTrait;
use tracing::{info, warn};

use crate::ingest::{self, EvalEdgeAccumulator};
use crate::messages::{IngestBatch, IngestReport, Transition, TransitionReport};
use crate::{known, transition};

/// How long a caller waits for the actor to exist after a restart.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a caller waits for its reply. Long on purpose: a session behind a
/// burst must block its reader (TCP backpressure on the worker), not drop the
/// batch; the per-transaction budget below bounds the actor's own work.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(600);
/// A transaction past this is rolled back and its caller told.
pub const GRAPH_TX_BUDGET: Duration = Duration::from_secs(120);
/// Queued ingest batches are flushed early once they carry this many derivations.
pub const INGEST_ROW_BUDGET: usize = 5000;
pub const HEALTH_NAME: &str = "graph";

type Reply<T> = RpcReplyPort<anyhow::Result<T>>;

pub enum GraphMsg {
    Ingest(IngestBatch, Reply<IngestReport>),
    KnownDerivations {
        drv_hashes: Vec<String>,
        reply: Reply<Vec<String>>,
    },
    Transition(Transition, Reply<TransitionReport>),
    Flush,
}

pub struct GraphArgs {
    pub ctx: DbContext,
    pub health: Option<Arc<SupervisorHealth>>,
}

pub struct GraphState {
    ctx: DbContext,
    health: Option<Arc<SupervisorHealth>>,
    edges: HashMap<EvaluationId, EvalEdgeAccumulator>,
    queued: Vec<(IngestBatch, Reply<IngestReport>)>,
    queued_rows: usize,
    flush_pending: bool,
}

impl GraphState {
    fn record(&self, outcome: &anyhow::Result<()>) {
        let Some(health) = &self.health else {
            return;
        };
        health.with(HEALTH_NAME, |h| match outcome {
            Ok(()) => h.last_ok_at = Some(Instant::now()),
            Err(e) => {
                h.pass_errors += 1;
                h.last_error = Some(e.to_string());
            }
        });
    }
}

pub struct GraphActor;

impl Actor for GraphActor {
    type Msg = GraphMsg;
    type State = GraphState;
    type Arguments = GraphArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(GraphState {
            ctx: args.ctx,
            health: args.health,
            edges: HashMap::new(),
            queued: Vec::new(),
            queued_rows: 0,
            flush_pending: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GraphMsg::Ingest(batch, reply) => {
                st.queued_rows += batch.derivations.len();
                st.queued.push((batch, reply));
                if st.queued_rows >= INGEST_ROW_BUDGET {
                    flush(st).await;
                } else if !st.flush_pending {
                    st.flush_pending = true;
                    let _ = myself.send_message(GraphMsg::Flush);
                }
            }
            GraphMsg::Flush => {
                st.flush_pending = false;
                flush(st).await;
            }
            GraphMsg::KnownDerivations { drv_hashes, reply } => {
                flush(st).await;
                let result = known::prunable(&st.ctx.worker_db, drv_hashes)
                    .await
                    .map_err(Into::into);
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(result);
            }
            GraphMsg::Transition(t, reply) => {
                flush(st).await;
                let GraphState { ctx, edges, .. } = st;
                let result = transact(ctx, GRAPH_TX_BUDGET, async |scoped| {
                    transition::apply(scoped, edges, t).await
                })
                .await;
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(result);
            }
        }

        Ok(())
    }
}

/// Write every queued batch in one transaction, a savepoint each, then reply
/// to all of them and run the post-commit effects of the ones that landed. A
/// batch that fails is lost (the wire has no ack the worker could retry on),
/// so its evaluation is failed rather than left with a hole in its graph.
async fn flush(st: &mut GraphState) {
    if st.queued.is_empty() {
        return;
    }

    let (batches, replies): (Vec<IngestBatch>, Vec<Reply<IngestReport>>) =
        std::mem::take(&mut st.queued).into_iter().unzip();
    st.queued_rows = 0;
    let GraphState { ctx, edges, .. } = st;
    let batches_ref = &batches;
    let written = transact(ctx, GRAPH_TX_BUDGET, async |scoped| {
        let mut outcomes = Vec::with_capacity(batches_ref.len());
        for batch in batches_ref {
            outcomes.push(ingest_one(scoped, edges, batch).await);
        }

        Ok(outcomes)
    })
    .await;

    match written {
        Ok(outcomes) => {
            st.record(&Ok(()));
            for ((batch, reply), outcome) in batches.into_iter().zip(replies).zip(outcomes) {
                match outcome {
                    Ok(report) => {
                        ingest::after_commit(&st.ctx, &batch, &report).await;
                        let _ = reply.send(Ok(report));
                    }
                    Err(e) => {
                        ingest::fail_evaluation(&st.ctx, batch.evaluation, &e.to_string()).await;
                        let _ = reply.send(Err(e));
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, batches = replies.len(), "ingest transaction failed; its evaluations are failed");
            st.record(&Err(anyhow!("{e}")));
            for (batch, reply) in batches.into_iter().zip(replies) {
                st.edges.remove(&batch.evaluation);
                ingest::fail_evaluation(&st.ctx, batch.evaluation, &e.to_string()).await;
                let _ = reply.send(Err(anyhow!("{e}")));
            }
        }
    }
}

/// One batch under its own savepoint, so a bad batch fails only its caller.
/// On failure the evaluation's accumulator is dropped with the rolled-back
/// rows, so no later flush references ids that never landed.
async fn ingest_one(
    scoped: &DbContext,
    edges: &mut HashMap<EvaluationId, EvalEdgeAccumulator>,
    batch: &IngestBatch,
) -> anyhow::Result<IngestReport> {
    let savepoint = Arc::new(scoped.worker_db.begin().await.context("savepoint")?);
    let inner = scoped.in_transaction(Arc::clone(&savepoint));
    let outcome = ingest::apply_batch(&inner, edges, batch).await;
    drop(inner);
    let savepoint =
        Arc::try_unwrap(savepoint).map_err(|_| anyhow!("a savepoint handle escaped its batch"))?;
    match outcome {
        Ok(report) => {
            savepoint.commit().await.context("release savepoint")?;
            Ok(report)
        }
        Err(e) => {
            edges.remove(&batch.evaluation);
            let _ = savepoint.rollback().await;
            Err(e)
        }
    }
}

/// `begin`, `work`, `commit`; a failure or a run past `budget` rolls back.
pub async fn transact<T, F>(ctx: &DbContext, budget: Duration, work: F) -> anyhow::Result<T>
where
    F: AsyncFnOnce(&DbContext) -> anyhow::Result<T>,
{
    let tx = Arc::new(ctx.worker_db.begin().await.context("begin")?);
    let scoped = ctx.in_transaction(Arc::clone(&tx));
    let outcome = tokio::time::timeout(budget, work(&scoped)).await;
    drop(scoped);
    let tx =
        Arc::try_unwrap(tx).map_err(|_| anyhow!("a transaction handle escaped its message"))?;
    match outcome {
        Ok(Ok(value)) => {
            tx.commit().await.context("commit")?;
            Ok(value)
        }
        Ok(Err(e)) => {
            let _ = tx.rollback().await;
            Err(e)
        }
        Err(_) => {
            let _ = tx.rollback().await;
            info!(
                budget_secs = budget.as_secs(),
                "graph transaction rolled back past its budget"
            );
            Err(anyhow!("graph transaction exceeded {}s", budget.as_secs()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use gradient_db::{NoReactor, WebDb, WorkerDb};
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_storage::{FileLogStorage, NarStore, StorageCtx};
    use gradient_types::*;
    use gradient_util::shutdown::Shutdown;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

    async fn ctx(db: DatabaseConnection) -> (DbContext, WorkerDb) {
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

    fn evaluation(id: EvaluationId) -> MEvaluation {
        MEvaluation {
            id,
            status: EvaluationStatus::EvaluatingDerivation,
            ..Default::default()
        }
    }

    fn batch(evaluation: EvaluationId) -> IngestBatch {
        IngestBatch {
            evaluation,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn queued_batches_share_one_transaction_and_a_query_after_them_sees_them() {
        let e1 = EvaluationId::now_v7();
        let e2 = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation(e1)], vec![evaluation(e2)]])
            .append_query_results([Vec::<MDerivation>::new()])
            .into_connection();
        let (ctx, pool) = ctx(db).await;
        let graph = crate::Graph::new();
        let actor = graph.spawn(ctx, None, None).await.unwrap();

        // Three messages land in the mailbox before the actor runs any of them,
        // so the query is processed while both batches are still queued.
        let (tx1, rx1) = ractor::concurrency::oneshot();
        let (tx2, rx2) = ractor::concurrency::oneshot();
        let (tx3, rx3) = ractor::concurrency::oneshot();
        actor
            .send_message(GraphMsg::Ingest(batch(e1), tx1.into()))
            .unwrap();
        actor
            .send_message(GraphMsg::Ingest(batch(e2), tx2.into()))
            .unwrap();
        actor
            .send_message(GraphMsg::KnownDerivations {
                drv_hashes: vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()],
                reply: tx3.into(),
            })
            .unwrap();
        assert_eq!(rx1.await.unwrap().unwrap().evaluation, e1);
        assert_eq!(rx2.await.unwrap().unwrap().evaluation, e2);
        assert!(rx3.await.unwrap().unwrap().is_empty());

        actor.stop_and_wait(None, None).await.unwrap();
        drop(actor);
        let log = pool.into_transaction_log();
        let rendered: Vec<String> = log.iter().map(|t| format!("{t:?}")).collect();
        let ingest_tx = rendered
            .iter()
            .position(|t| t.contains(r#"FROM \"evaluation\""#))
            .unwrap_or_else(|| panic!("the ingest transaction is logged: {rendered:?}"));
        assert_eq!(
            rendered[ingest_tx]
                .matches(r#"FROM \"evaluation\""#)
                .count(),
            2,
            "both batches in one transaction: {rendered:?}"
        );
        let known = rendered
            .iter()
            .position(|t| t.contains(r#"FROM \"derivation\""#))
            .unwrap_or_else(|| panic!("the known-derivations read is logged: {rendered:?}"));
        assert!(
            known > ingest_tx,
            "the read runs after the writes: {rendered:?}"
        );
    }

    #[tokio::test]
    async fn a_batch_without_its_evaluation_fails_only_its_caller() {
        let e1 = EvaluationId::now_v7();
        let e2 = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation(e1)], Vec::<MEvaluation>::new()])
            .into_connection();
        let (ctx, _) = ctx(db).await;
        let graph = crate::Graph::new();
        let actor = graph.spawn(ctx, None, None).await.unwrap();

        let (tx1, rx1) = ractor::concurrency::oneshot();
        let (tx2, rx2) = ractor::concurrency::oneshot();
        actor
            .send_message(GraphMsg::Ingest(batch(e1), tx1.into()))
            .unwrap();
        actor
            .send_message(GraphMsg::Ingest(batch(e2), tx2.into()))
            .unwrap();
        assert!(rx1.await.unwrap().is_ok());
        let err = rx2.await.unwrap().expect_err("no evaluation row");
        assert!(err.to_string().contains("not found"), "{err}");
        actor.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn a_transaction_past_its_budget_is_rolled_back() {
        let (ctx, pool) = ctx(MockDatabase::new(DatabaseBackend::Postgres).into_connection()).await;
        let err = transact(&ctx, Duration::from_millis(20), async |_scoped| {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(())
        })
        .await
        .expect_err("past the budget");
        assert!(err.to_string().contains("exceeded"), "{err}");
        drop(ctx);
        let log: Vec<String> = pool
            .into_transaction_log()
            .iter()
            .map(|t| format!("{t:?}"))
            .collect();
        assert!(
            log.iter().any(|t| t.contains("ROLLBACK")) && log.iter().all(|t| !t.contains("COMMIT")),
            "rolled back, never committed: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_respawned_actor_answers_the_call_that_waited_for_it() {
        let (ctx, _) = ctx(MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MDerivation>::new()])
            .into_connection())
        .await;
        let graph = crate::Graph::new();
        let first = graph.spawn(ctx.clone(), None, None).await.unwrap();
        first.stop_and_wait(None, None).await.unwrap();
        graph.actor.send_replace(None);

        let waiting = {
            let graph = Arc::clone(&graph);
            ctx.shutdown
                .spawn(async move { graph.known_derivations(vec!["a".into()]).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let second = graph.spawn(ctx, None, None).await.unwrap();
        assert!(waiting.await.unwrap().unwrap().is_empty());
        second.stop_and_wait(None, None).await.unwrap();
    }
}
