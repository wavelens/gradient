/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one writer of the dependency graph and the cache index. Every message is
//! one transaction; ingest batches and NAR commits queued together are one
//! transaction with a savepoint each, and any other message flushes that queue
//! first, so it acts on its callers' earlier writes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use gradient_db::DbContext;
use gradient_types::DerivationId;
use gradient_util::supervision::SupervisorHealth;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use sea_orm::{ConnectionTrait, TransactionTrait};
use std::collections::HashMap;
use tracing::{info, warn};

use crate::ingest;
use crate::messages::{
    DemoteReport, Demotion, GcReport, GcRequest, IngestBatch, IngestReport, NarCommit,
    NarCommitted, RequeueScope, Transition, TransitionReport, UpstreamHit,
};
use crate::{demote, gc, nar, requeue, transition};

/// How long a caller waits for the actor to exist after a restart.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A transaction past this is rolled back and its caller told. This is the only
/// bound on a reply: a caller waits out the queue ahead of its message, because
/// the actor still runs a message whose caller gave up, and a session behind a
/// burst must block its reader (TCP backpressure on the worker), not drop the batch.
/// Postgres enforces it per statement as well: the rollback waits for the running
/// statement to end, so a budget the database does not know is no bound at all.
pub const GRAPH_TX_BUDGET: Duration = Duration::from_secs(120);
/// How many times a transaction aborted for a deadlock or serialization failure runs.
pub const GRAPH_TX_ATTEMPTS: u32 = 3;
/// Queued ingest batches are flushed early once they carry this many derivations.
pub const INGEST_ROW_BUDGET: usize = 5000;
/// Queued NAR commits are flushed early once this many wait.
pub const NAR_COMMIT_BUDGET: usize = 32;
/// A flush stops taking NAR commits once it has run this long and leaves the rest
/// for the next one. It holds the anchor locks of every commit in it until it ends,
/// so this bounds how long a dispatch claim waits behind it, whatever the commits
/// cost, while still sharing the WAL flush among the ones that fit.
pub const NAR_FLUSH_TIME: Duration = Duration::from_millis(100);
pub const HEALTH_NAME: &str = "graph";

type Reply<T> = RpcReplyPort<anyhow::Result<T>>;

pub enum GraphMsg {
    Ingest(IngestBatch, Reply<IngestReport>),
    UpstreamHits(HashMap<String, UpstreamHit>, Reply<()>),
    UpstreamProbed(Vec<DerivationId>, Reply<()>),
    CommitNar(NarCommit, Reply<NarCommitted>),
    Transition(Transition, Reply<TransitionReport>),
    Requeue(RequeueScope, Reply<u64>),
    Demote(Demotion, Reply<DemoteReport>),
    Gc(GcRequest, Reply<GcReport>),
    Flush,
}

pub struct GraphArgs {
    pub ctx: DbContext,
    pub health: Option<Arc<SupervisorHealth>>,
}

pub struct GraphState {
    ctx: DbContext,
    health: Option<Arc<SupervisorHealth>>,
    queued: Vec<(IngestBatch, Reply<IngestReport>)>,
    queued_rows: usize,
    nars: Vec<(NarCommit, Reply<NarCommitted>)>,
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
            queued: Vec::new(),
            queued_rows: 0,
            nars: Vec::new(),
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
                queue_flush(&myself, st).await;
            }
            GraphMsg::Flush => {
                st.flush_pending = false;
                flush(&myself, st).await;
            }
            GraphMsg::UpstreamHits(hits, reply) => {
                flush(&myself, st).await;
                let hits = &hits;
                let result = transact(&st.ctx, GRAPH_TX_BUDGET, move |scoped| async move {
                    ingest::apply_upstream_hits(&scoped, hits).await
                })
                .await;
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(ask_probe(st, result));
            }
            GraphMsg::UpstreamProbed(anchors, reply) => {
                flush(&myself, st).await;
                let anchors = &anchors;
                let result = transact(&st.ctx, GRAPH_TX_BUDGET, move |scoped| async move {
                    ingest::mark_probed(&scoped, anchors).await
                })
                .await;
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(ask_probe(st, result));
            }
            GraphMsg::CommitNar(commit, reply) => {
                st.nars.push((commit, reply));
                queue_flush(&myself, st).await;
            }
            GraphMsg::Transition(t, reply) => {
                flush(&myself, st).await;
                let t = &t;
                let result = transact(&st.ctx, GRAPH_TX_BUDGET, move |scoped| async move {
                    transition::apply(&scoped, t.clone()).await
                })
                .await;
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(result);
            }
            GraphMsg::Requeue(scope, reply) => {
                flush(&myself, st).await;
                let scope = &scope;
                let result = transact(&st.ctx, GRAPH_TX_BUDGET, move |scoped| async move {
                    requeue::apply(&scoped, *scope).await
                })
                .await;
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(result);
            }
            GraphMsg::Demote(demotion, reply) => {
                flush(&myself, st).await;
                let demotion = &demotion;
                let result = transact(&st.ctx, GRAPH_TX_BUDGET, move |scoped| async move {
                    demote::apply(&scoped, demotion.clone()).await
                })
                .await;
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(result);
            }
            GraphMsg::Gc(request, reply) => {
                flush(&myself, st).await;
                let request = &request;
                let result = transact(&st.ctx, GRAPH_TX_BUDGET, move |scoped| async move {
                    gc::apply(&scoped, request.clone()).await
                })
                .await;
                st.record(&result.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}")));
                let _ = reply.send(result);
            }
        }

        Ok(())
    }
}

/// Hand a committed round's gained demand to the upstream probe, and turn the
/// result into the caller's. Post-commit on purpose: the probe reads the rows on
/// its own connection, and an anchor asked for before its transaction lands plans
/// to nothing and is then remembered as asked for five minutes.
fn ask_probe(st: &GraphState, result: anyhow::Result<Vec<DerivationId>>) -> anyhow::Result<()> {
    result.map(|gained| st.ctx.probe_requests.send(gained))
}

/// Flush now once the queue is past a budget, otherwise after the messages
/// already in the mailbox, so a burst of them shares one transaction.
async fn queue_flush(myself: &ActorRef<GraphMsg>, st: &mut GraphState) {
    if st.queued_rows >= INGEST_ROW_BUDGET || st.nars.len() >= NAR_COMMIT_BUDGET {
        flush(myself, st).await;
    } else if !st.flush_pending {
        st.flush_pending = true;
        let _ = myself.send_message(GraphMsg::Flush);
    }
}

/// Write every queued batch and NAR commit in one transaction, a savepoint
/// each, then reply to all of them and run the post-commit effects of the ones
/// that landed. A batch that fails is lost (the wire has no ack the worker could
/// retry on), so its evaluation is failed rather than left with a hole in its
/// graph; a failed commit is its uploader's error to report.
#[tracing::instrument(level = "debug", skip_all, fields(batches = st.queued.len(), rows = st.queued_rows, nars = st.nars.len()))]
async fn flush(myself: &ActorRef<GraphMsg>, st: &mut GraphState) {
    if st.queued.is_empty() && st.nars.is_empty() {
        return;
    }

    let (batches, replies): (Vec<IngestBatch>, Vec<Reply<IngestReport>>) =
        std::mem::take(&mut st.queued).into_iter().unzip();
    let (commits, commit_replies): (Vec<NarCommit>, Vec<Reply<NarCommitted>>) =
        std::mem::take(&mut st.nars).into_iter().unzip();
    st.queued_rows = 0;
    let (batches_ref, commits_ref) = (&batches, &commits);
    let written = transact(&st.ctx, GRAPH_TX_BUDGET, move |scoped| async move {
        let mut ingested = Vec::with_capacity(batches_ref.len());
        for batch in batches_ref {
            ingested.push(escalate_retryable(ingest_one(&scoped, batch).await)?);
        }

        let committed = commit_nars(&scoped, commits_ref).await?;
        Ok((ingested, committed))
    })
    .await;

    match written {
        Ok((outcomes, committed)) => {
            st.record(&Ok(()));
            let mut landed: Vec<_> = commits.into_iter().zip(commit_replies).collect();
            let deferred = landed.split_off(committed.len());
            for ((commit, reply), outcome) in landed.into_iter().zip(committed) {
                if let Ok(done) = &outcome {
                    nar::after_commit(&st.ctx, done, &commit.store_path);
                }
                let _ = reply.send(outcome);
            }
            if !deferred.is_empty() {
                st.nars.splice(0..0, deferred);
                if !st.flush_pending {
                    st.flush_pending = true;
                    let _ = myself.send_message(GraphMsg::Flush);
                }
            }
            for ((batch, reply), outcome) in batches.into_iter().zip(replies).zip(outcomes) {
                match outcome {
                    Ok(report) => {
                        ingest::after_commit(&st.ctx, myself, &batch, &report).await;
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
            warn!(error = %e, batches = replies.len(), nars = commit_replies.len(), "ingest transaction failed; its evaluations are failed");
            st.record(&Err(anyhow!("{e}")));
            for reply in commit_replies {
                let _ = reply.send(Err(anyhow!("{e}")));
            }
            for (batch, reply) in batches.into_iter().zip(replies) {
                ingest::fail_evaluation(&st.ctx, batch.evaluation, &e.to_string()).await;
                let _ = reply.send(Err(anyhow!("{e}")));
            }
        }
    }
}

/// A batch that failed for its timing fails the whole flush, which `transact` then
/// retries; one that failed for its content stays that batch's own outcome. Its
/// savepoint already rolled back, but a deadlock victim is no verdict on the batch.
fn escalate_retryable<T>(outcome: anyhow::Result<T>) -> anyhow::Result<anyhow::Result<T>> {
    match outcome {
        Err(e) if is_retryable(&e) => Err(e),
        outcome => Ok(outcome),
    }
}

/// One batch under its own savepoint, so a bad batch fails only its caller.
#[tracing::instrument(level = "debug", skip_all)]
async fn ingest_one(scoped: &DbContext, batch: &IngestBatch) -> anyhow::Result<IngestReport> {
    in_savepoint(scoped, |inner| async move {
        ingest::apply_batch(&inner, batch).await
    })
    .await
}

/// The queued NARs in one set-based savepoint; if that fails, one by one, so a bad
/// commit fails only its uploader, stopping at [`NAR_FLUSH_TIME`] and leaving the
/// rest to the next flush. A deadlock fails the whole flush for `transact` to retry.
#[tracing::instrument(level = "debug", skip_all, fields(nars = commits.len()))]
async fn commit_nars(
    scoped: &DbContext,
    commits: &[NarCommit],
) -> anyhow::Result<Vec<anyhow::Result<NarCommitted>>> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    match in_savepoint(scoped, |inner| async move {
        nar::commit_batch(&inner, commits).await
    })
    .await
    {
        Ok(done) => return Ok(done.into_iter().map(Ok).collect()),
        Err(e) if is_retryable(&e) => return Err(e),
        Err(e) => {
            warn!(error = %e, nars = commits.len(), "NAR batch failed; committing one by one")
        }
    }

    let started = Instant::now();
    let mut committed = Vec::with_capacity(commits.len());
    for commit in commits {
        if !committed.is_empty() && started.elapsed() >= NAR_FLUSH_TIME {
            break;
        }
        committed.push(escalate_retryable(commit_one(scoped, commit).await)?);
    }
    Ok(committed)
}

/// One NAR commit under its own savepoint, so a bad commit fails only its uploader.
#[tracing::instrument(level = "debug", skip_all)]
async fn commit_one(scoped: &DbContext, commit: &NarCommit) -> anyhow::Result<NarCommitted> {
    in_savepoint(
        scoped,
        |inner| async move { nar::commit(&inner, commit).await },
    )
    .await
}

async fn in_savepoint<T, F, Fut>(scoped: &DbContext, work: F) -> anyhow::Result<T>
where
    F: FnOnce(DbContext) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let savepoint = Arc::new(scoped.worker_db.begin().await.context("savepoint")?);
    let outcome = work(scoped.in_transaction(Arc::clone(&savepoint))).await;
    let savepoint =
        Arc::try_unwrap(savepoint).map_err(|_| anyhow!("a savepoint handle escaped its batch"))?;
    match outcome {
        Ok(report) => {
            savepoint.commit().await.context("release savepoint")?;
            Ok(report)
        }
        Err(e) => {
            let _ = savepoint.rollback().await;
            Err(e)
        }
    }
}

/// `begin`, `work`, `commit`; a failure or a run past `budget` rolls back. A deadlock
/// or serialization failure rolls back and runs `work` again, up to
/// [`GRAPH_TX_ATTEMPTS`] times: the advisory anchor keys make two graph writers wait
/// on each other instead of missing each other's rows, and Postgres breaks the rare
/// cycle that creates by aborting one of them. Board events and probe requests a
/// failed attempt already sent are not recalled, so a retry sends them again.
pub async fn transact<T, F, Fut>(ctx: &DbContext, budget: Duration, work: F) -> anyhow::Result<T>
where
    F: Fn(DbContext) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut attempt = 1;
    loop {
        match transact_once(ctx, budget, &work).await {
            Err(e) if attempt < GRAPH_TX_ATTEMPTS && is_retryable(&e) => {
                warn!(attempt, error = %e, "graph transaction retried");
                attempt += 1;
            }
            outcome => return outcome,
        }
    }
}

fn statement_timeout(budget: Duration) -> String {
    format!("SET LOCAL statement_timeout = {}", budget.as_millis())
}

/// SQLSTATE `40P01` (deadlock detected) or `40001` (serialization failure) anywhere
/// in the chain: the transaction was aborted for its timing, not its content.
fn is_retryable(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let Some(
            sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(e))
            | sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(e))
            | sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(e)),
        ) = cause.downcast_ref::<sea_orm::DbErr>()
        else {
            return false;
        };

        e.as_database_error()
            .and_then(|d| d.code())
            .is_some_and(|c| c == "40P01" || c == "40001")
    })
}

#[tracing::instrument(level = "debug", skip_all)]
async fn transact_once<T, F, Fut>(ctx: &DbContext, budget: Duration, work: &F) -> anyhow::Result<T>
where
    F: Fn(DbContext) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let tx = Arc::new(ctx.worker_db.begin().await.context("begin")?);
    tx.execute_unprepared(&statement_timeout(budget))
        .await
        .context("statement timeout")?;
    let scoped = ctx.in_transaction(Arc::clone(&tx));
    let ready_set = scoped.ready_set.clone();
    let outcome = tokio::time::timeout(budget, work(scoped)).await;
    let tx =
        Arc::try_unwrap(tx).map_err(|_| anyhow!("a transaction handle escaped its message"))?;
    match outcome {
        Ok(Ok(value)) => {
            tx.commit().await.context("commit")?;
            // The transaction may have written outbox rows; the effects actor
            // claims them now rather than on its next tick.
            ctx.outbox_wake.notify_one();
            ready_set.publish();
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
    use crate::test_ctx::ctx;
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_types::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn timeouts(transactions: usize) -> Vec<MockExecResult> {
        vec![MockExecResult::default(); transactions]
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
    async fn queued_batches_share_one_transaction() {
        let e1 = EvaluationId::now_v7();
        let e2 = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
            .append_query_results([vec![evaluation(e1)], vec![evaluation(e2)]])
            .into_connection();
        let (ctx, pool) = ctx(db).await;
        let graph = crate::Graph::new();
        let actor = graph.spawn(ctx, None, None).await.unwrap();

        // Both land in the mailbox before the actor runs either, so the flush the
        // first one queues comes after the second.
        let (tx1, rx1) = ractor::concurrency::oneshot();
        let (tx2, rx2) = ractor::concurrency::oneshot();
        actor
            .send_message(GraphMsg::Ingest(batch(e1), tx1.into()))
            .unwrap();
        actor
            .send_message(GraphMsg::Ingest(batch(e2), tx2.into()))
            .unwrap();
        assert_eq!(rx1.await.unwrap().unwrap().evaluation, e1);
        assert_eq!(rx2.await.unwrap().unwrap().evaluation, e2);

        actor.stop_and_wait(None, None).await.unwrap();
        drop((actor, graph));
        let rendered: Vec<String> = pool
            .into_transaction_log()
            .iter()
            .map(|t| format!("{t:?}"))
            .collect();
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
    }

    fn nar(hash: &str) -> NarCommit {
        NarCommit {
            store_path: format!("/nix/store/{hash}-hello-2.12"),
            file_hash: "sha256:abc".into(),
            file_size: 5,
            nar_size: 5,
            nar_hash: "sha256:def".into(),
            references: Vec::new(),
            deriver: None,
            ca: None,
            targets: crate::messages::SignTargets::None,
            confirmed: true,
        }
    }

    fn cached_path(hash: &str) -> MCachedPath {
        MCachedPath {
            id: gradient_types::ids::CachedPathId::now_v7(),
            hash: hash.into(),
            package: "hello-2.12".into(),
            created_at: now(),
            ..Default::default()
        }
    }

    /// An upload burst is a mailbox of commits; each one committing alone paid a
    /// round trip and a WAL flush per NAR, and serialised the burst behind them.
    #[tokio::test]
    async fn queued_nar_commits_share_one_transaction() {
        let (h1, h2) = (
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        let none = Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([none(), none()])
            .append_query_results([Vec::<MDerivationOutput>::new()])
            .append_exec_results([MockExecResult::default()])
            .into_connection();
        let (ctx, pool) = ctx(db).await;
        let graph = crate::Graph::new();
        let actor = graph.spawn(ctx, None, None).await.unwrap();

        let (tx1, rx1) = ractor::concurrency::oneshot();
        let (tx2, rx2) = ractor::concurrency::oneshot();
        actor
            .send_message(GraphMsg::CommitNar(nar(h1), tx1.into()))
            .unwrap();
        actor
            .send_message(GraphMsg::CommitNar(nar(h2), tx2.into()))
            .unwrap();
        assert!(rx1.await.unwrap().unwrap().created);
        assert!(rx2.await.unwrap().unwrap().created);

        actor.stop_and_wait(None, None).await.unwrap();
        drop((actor, graph));
        let statements = gradient_db::pool::raw_statements(pool.into_transaction_log());
        let inserts: Vec<String> = statements
            .iter()
            .filter(|s| s.sql.contains(r#"INSERT INTO "cached_path""#))
            .map(|s| format!("{:?}", s.values))
            .collect();
        assert_eq!(inserts.len(), 1, "one insert for the batch: {statements:?}");
        assert!(
            inserts[0].contains(h1) && inserts[0].contains(h2),
            "{inserts:?}"
        );
    }

    /// One bad NAR must not fail the uploads it was batched with.
    #[tokio::test]
    async fn a_failed_batch_commits_its_nars_one_by_one() {
        let good = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let none = Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![cached_path(good)]])
            .append_query_results([none(), none()])
            .append_exec_results([MockExecResult::default()])
            .into_connection();
        let (ctx, _pool) = ctx(db).await;
        let graph = crate::Graph::new();
        let actor = graph.spawn(ctx, None, None).await.unwrap();

        let (tx1, rx1) = ractor::concurrency::oneshot();
        let (tx2, rx2) = ractor::concurrency::oneshot();
        actor
            .send_message(GraphMsg::CommitNar(nar(good), tx1.into()))
            .unwrap();
        actor
            .send_message(GraphMsg::CommitNar(
                NarCommit {
                    store_path: "/nix/store/EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE-hello".into(),
                    ..nar(good)
                },
                tx2.into(),
            ))
            .unwrap();
        assert!(rx1.await.unwrap().unwrap().created);
        let err = rx2.await.unwrap().expect_err("malformed");
        assert!(err.to_string().contains("malformed"), "{err}");
        actor.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn a_batch_without_its_evaluation_fails_only_its_caller() {
        let e1 = EvaluationId::now_v7();
        let e2 = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
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
        let (ctx, pool) = ctx(MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
            .into_connection())
        .await;
        let err = transact(&ctx, Duration::from_millis(20), |_scoped| async {
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

    /// The budget reaches Postgres before any work, so a runaway statement is
    /// cancelled there: a dropped future leaves it running, and the rollback
    /// waited 18 minutes for one while every caller queued behind the actor.
    #[tokio::test]
    async fn a_transaction_hands_its_budget_to_postgres_first() {
        let (ctx, pool) = ctx(MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
            .into_connection())
        .await;
        transact(&ctx, Duration::from_secs(120), |_scoped| async { Ok(()) })
            .await
            .unwrap();
        drop(ctx);
        let log: Vec<String> = pool
            .into_transaction_log()
            .iter()
            .map(|t| format!("{t:?}"))
            .collect();
        assert!(
            log[0].contains("SET LOCAL statement_timeout = 120000"),
            "{log:?}"
        );
    }

    #[derive(Debug)]
    struct Coded(&'static str);

    impl std::fmt::Display for Coded {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Coded {}

    impl sea_orm::sqlx::error::DatabaseError for Coded {
        fn message(&self) -> &str {
            self.0
        }

        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.0))
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> sea_orm::sqlx::error::ErrorKind {
            sea_orm::sqlx::error::ErrorKind::Other
        }
    }

    fn coded(code: &'static str) -> anyhow::Error {
        anyhow::Error::new(sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(
            Arc::new(sea_orm::sqlx::Error::Database(Box::new(Coded(code)))),
        )))
    }

    #[test]
    fn a_deadlock_and_a_serialization_failure_retry_through_context() {
        for code in ["40P01", "40001"] {
            assert!(is_retryable(&coded(code).context("seed")), "{code}");
        }
    }

    #[test]
    fn anything_else_does_not_retry() {
        assert!(!is_retryable(&coded("23505")));
        assert!(!is_retryable(&anyhow!("graph transaction exceeded 120s")));
    }

    #[tokio::test]
    async fn a_deadlocked_transaction_is_retried_and_its_second_attempt_commits() {
        let (ctx, pool) = ctx(MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(2))
            .into_connection())
        .await;
        let attempts = &std::sync::atomic::AtomicU32::new(0);
        let out = transact(&ctx, GRAPH_TX_BUDGET, move |_scoped| async move {
            if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Err(coded("40P01"));
            }
            Ok(7)
        })
        .await
        .unwrap();
        assert_eq!(out, 7);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        drop(ctx);
        let log: Vec<String> = pool
            .into_transaction_log()
            .iter()
            .map(|t| format!("{t:?}"))
            .collect();
        assert!(
            log.iter().any(|t| t.contains("ROLLBACK")) && log.iter().any(|t| t.contains("COMMIT")),
            "the first attempt rolled back, the second committed: {log:?}"
        );
    }

    #[test]
    fn a_batch_that_deadlocked_fails_the_flush_so_it_is_retried() {
        assert!(escalate_retryable::<()>(Err(coded("40P01").context("seed"))).is_err());
    }

    #[test]
    fn a_batch_that_failed_for_its_content_fails_only_itself() {
        let outcome = escalate_retryable::<()>(Err(coded("23505"))).expect("stays the batch's own");
        assert!(outcome.is_err());
        assert!(
            escalate_retryable(Ok(IngestReport::default()))
                .unwrap()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_unique_violation_is_not_retried() {
        let (ctx, _pool) = ctx(MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
            .into_connection())
        .await;
        let attempts = &std::sync::atomic::AtomicU32::new(0);
        let err = transact(&ctx, GRAPH_TX_BUDGET, move |_scoped| async move {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::Result::<()>::Err(coded("23505"))
        })
        .await;
        assert!(err.is_err());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_respawned_actor_answers_the_call_that_waited_for_it() {
        let (ctx, _) = ctx(MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(timeouts(1))
            .into_connection())
        .await;
        let graph = crate::Graph::new();
        let first = graph.spawn(ctx.clone(), None, None).await.unwrap();
        first.stop_and_wait(None, None).await.unwrap();
        graph.actor.send_replace(None);

        let waiting = {
            let graph = Arc::clone(&graph);
            ctx.shutdown
                .spawn(async move { graph.upstream_hits(Default::default()).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let second = graph.spawn(ctx, None, None).await.unwrap();
        waiting.await.unwrap().unwrap();
        second.stop_and_wait(None, None).await.unwrap();
    }
}
