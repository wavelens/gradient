/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::status::TransitionChange;

use super::fetchable::{blocking_dependency_count, mark_fetchable, mark_unfetchable};
use super::lock::{SharedBuildLock, ids, lock_shared_builds};
use super::queue::{PROMOTE_ANY_QUERY, UNPROMOTE_UNGATED_QUERY};
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use std::sync::LazyLock;

fn repair_scope() -> String {
    let pending = crate::sql::status::build_in(&BuildStatus::PENDING);
    let terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS);
    format!(
        "SELECT q.derivation FROM derivation_build q WHERE q.status IN ({pending}) \
       UNION \
         SELECT e.dependency FROM derivation_dependency e \
         JOIN derivation_build q ON q.derivation = e.derivation \
         WHERE q.status IN ({pending}) \
       UNION \
         SELECT q.derivation FROM derivation_build q \
         WHERE q.fetchable AND q.missing_runtime_deps > 0 \
       UNION \
         SELECT q.derivation FROM derivation_build q \
         WHERE q.status IN ({terminal_success}) AND NOT q.fetchable"
    )
}

crate::sql_fn! {
    REPAIR_SCOPE_QUERY = repair_scope,
        params = [],
        tier = Sweep;
}

pub(super) static RECOUNT_BLOCKING_DEPS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET blocking_deps = x.n \
         FROM (SELECT p.derivation, p.blocking_deps AS old, {count} AS n \
               FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])) x \
         WHERE db.derivation = x.derivation AND db.blocking_deps = x.old AND x.old <> x.n",
        count = blocking_dependency_count("p"),
    )
});

crate::sql_lazy! {
    RECOUNT_BLOCKING_DEPS_QUERY = || RECOUNT_BLOCKING_DEPS.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

#[derive(Debug, Default)]
pub struct RepairedFetchable {
    pub marked: u64,
    pub unqueued: Vec<TransitionChange>,
}

#[derive(Debug, Default)]
pub struct Repaired {
    pub blocking_deps: u64,
    pub promoted: Vec<TransitionChange>,
    pub unpromoted: Vec<TransitionChange>,
}

pub async fn can_start_scope<C: ConnectionTrait>(db: &C) -> Result<Vec<DerivationId>, DbErr> {
    db.query_all_raw(REPAIR_SCOPE_QUERY.stmt())
        .await?
        .into_iter()
        .map(|r| {
            r.try_get::<uuid::Uuid>("", "derivation")
                .map(DerivationId::new)
        })
        .collect()
}

async fn recount_blocking_deps(lock: &SharedBuildLock<'_>) -> Result<u64, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(0);
    }

    Ok(lock
        .txn
        .execute_raw(RECOUNT_BLOCKING_DEPS_QUERY.bind([ids(&lock.derivations)]))
        .await?
        .rows_affected())
}

/// A column corrected without adjusting its parents' `blocking_deps` leaves them off by one for good.
/// Queueing what reached zero is left to [`repair_can_start`], after every counter is recounted.
pub async fn repair_fetchable<C>(db: &C, scope: &[DerivationId]) -> Result<RepairedFetchable, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut repaired = RepairedFetchable::default();
    for chunk in scope.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_shared_builds(&txn, chunk).await?;
        let fetchable = mark_fetchable(&lock).await?;
        let unfetchable = mark_unfetchable(&lock).await?;
        txn.commit().await?;

        repaired.marked += (fetchable.marked + unfetchable.marked.len()) as u64;
        repaired.unqueued.extend(unfetchable.unqueued);
    }

    Ok(repaired)
}

/// The un-promote is going first.
/// EvalPlanQual is not re-evaluating the gate's `EXISTS` subqueries.
/// A promote can still fire on a `.drv` retired during a lock wait.
pub async fn repair_can_start<C>(db: &C, scope: &[DerivationId]) -> Result<Repaired, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut blocking_deps = 0u64;
    for chunk in scope.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_shared_builds(&txn, chunk).await?;
        blocking_deps += recount_blocking_deps(&lock).await?;
        txn.commit().await?;
    }

    let unpromoted = returned_transitions(db.query_all_raw(UNPROMOTE_UNGATED_QUERY.stmt()).await?);
    let promoted = transitions_from(
        returned_derivations(db.query_all_raw(PROMOTE_ANY_QUERY.stmt()).await?),
        BuildStatus::Created,
        BuildStatus::Queued,
    );

    Ok(Repaired {
        blocking_deps,
        promoted,
        unpromoted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::test_rows::{drv, exec, norm, transition_row};
    use crate::pool::statements;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    fn startable_row(id: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("startable".to_owned(), Value::from(true)),
        ])
    }

    #[test]
    fn the_counter_recount_is_a_compare_and_swap_over_the_locked_chunk() {
        let sql = norm(&RECOUNT_BLOCKING_DEPS);
        assert!(
            sql.contains("FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])"),
            "the chunk is the whole bound: {sql}"
        );
        assert!(
            !sql.contains("WHERE p.status IN"),
            "a status scope would widen a recount past its lock: {sql}"
        );
        assert!(
            sql.contains("db.blocking_deps = x.old AND x.old <> x.n"),
            "compare-and-swap plus drift filter: {sql}"
        );
    }

    #[test]
    fn the_scope_reaches_one_edge_past_the_pending_shared_builds_and_every_contradicting_flag() {
        let sql = norm(&repair_scope());
        assert_eq!(sql.matches("q.status IN (0, 1)").count(), 2, "{sql}");
        assert!(
            sql.contains("SELECT e.dependency FROM derivation_dependency e"),
            "{sql}"
        );
        assert!(
            sql.starts_with("SELECT q.derivation"),
            "the UNION names the first arm's column: {sql}"
        );
        assert!(
            sql.contains("WHERE q.fetchable AND q.missing_runtime_deps > 0"),
            "a fetchable shared build with a missing runtime dependency counted is a lie every walk trusts: {sql}"
        );
        assert!(
            sql.contains("WHERE q.status IN (3, 7) AND NOT q.fetchable"),
            "a terminal-success shared build without the flag may be complete again: {sql}"
        );
        assert!(
            !sql.contains("derivation_output"),
            "the scope reads columns only; the mark evaluates the predicate: {sql}"
        );
    }

    #[tokio::test]
    async fn a_repaired_column_adjusts_its_parents_in_the_same_transaction_and_queues_nothing() {
        let now_fetchable = DerivationId::now_v7();
        let unblocked_parent = DerivationId::now_v7();
        let no_longer_fetchable = DerivationId::now_v7();
        let unqueued_parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0)])
            .append_query_results([vec![drv(now_fetchable)]])
            .append_query_results([vec![startable_row(unblocked_parent)]])
            .append_query_results([vec![drv(no_longer_fetchable)]])
            .append_query_results([vec![transition_row(unqueued_parent, 1, 0)]])
            .into_connection();

        let repaired = repair_fetchable(&db, &[now_fetchable, no_longer_fetchable])
            .await
            .unwrap();

        assert_eq!(repaired.marked, 2);
        assert_eq!(repaired.unqueued.len(), 1);
        assert_eq!(repaired.unqueued[0].derivation, unqueued_parent);
        assert_eq!(
            (repaired.unqueued[0].from, repaired.unqueued[0].to),
            (BuildStatus::Queued, BuildStatus::Created)
        );

        let raw = db.into_transaction_log();
        assert_eq!(raw.len(), 1, "{raw:?}");
        let inside: Vec<&str> = raw[0].statements().iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(
            inside.len(),
            7,
            "BEGIN, lock, two marks with their parents, COMMIT: {inside:?}"
        );
        assert!(inside[1].contains("FOR NO KEY UPDATE"), "{inside:?}");
        assert!(
            inside[2].contains("SET fetchable = true") && inside[3].contains("blocking_deps - c.n"),
            "{inside:?}"
        );
        assert!(
            inside[4].contains("SET fetchable = false")
                && inside[5].contains("blocking_deps + c.n"),
            "{inside:?}"
        );
        assert!(
            !inside.iter().any(|s| s.contains("SET status = 1")),
            "a parent reaching zero is queued by repair_can_start, after its recount: {inside:?}"
        );
    }

    #[tokio::test]
    async fn the_repair_locks_each_chunk_before_it_recounts_it() {
        let a = DerivationId::now_v7();
        let demoted = DerivationId::now_v7();
        let promoted = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv(a)]])
            .append_exec_results([exec(0), exec(0), exec(3)])
            .append_query_results([empty(), empty()])
            .append_query_results([vec![transition_row(demoted, 1, 0)]])
            .append_query_results([vec![drv(promoted)]])
            .into_connection();

        let scope = can_start_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let repaired = repair_can_start(&db, &scope).await.unwrap();

        assert_eq!((fetchable.marked, repaired.blocking_deps), (0, 3));
        assert_eq!(repaired.unpromoted.len(), 1);
        assert_eq!(repaired.unpromoted[0].derivation, demoted);
        assert_eq!(repaired.promoted.len(), 1);
        assert_eq!(repaired.promoted[0].derivation, promoted);

        let raw = db.into_transaction_log();
        assert_eq!(
            raw.len(),
            5,
            "the scope select, one transaction per pass, then the queue: {raw:?}"
        );

        for (entry, first_write) in [(1, "SET fetchable = true"), (2, "SET blocking_deps = x.n")] {
            let inside: Vec<&str> = raw[entry]
                .statements()
                .iter()
                .map(|s| s.sql.as_str())
                .collect();
            assert!(
                inside[1].contains("ORDER BY derivation FOR NO KEY UPDATE"),
                "{inside:?}"
            );
            assert!(inside[2].contains(first_write), "{inside:?}");
        }

        let log = statements(raw);
        assert!(log[log.len() - 2].contains("SET status = 0"), "{log:?}");
        assert!(log[log.len() - 1].contains("SET status = 1"), "{log:?}");
    }

    #[tokio::test]
    async fn the_repair_finishes_fetchable_everywhere_before_it_recounts_a_counter() {
        let scope: Vec<BTreeMap<String, Value>> = (0..crate::IN_CHUNK_SIZE + 1)
            .map(|_| drv(DerivationId::now_v7()))
            .collect();
        let empty = Vec::<BTreeMap<String, Value>>::new;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([scope])
            .append_exec_results(vec![exec(1); 6])
            .append_query_results([empty(), empty(), empty(), empty(), empty(), empty()])
            .into_connection();

        let scope = can_start_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let repaired = repair_can_start(&db, &scope).await.unwrap();

        assert_eq!(
            (fetchable.marked, repaired.blocking_deps),
            (0, 2),
            "one recounted row per chunk"
        );

        let log = statements(db.into_transaction_log());
        let columns: Vec<&str> = log
            .iter()
            .filter_map(|s| {
                if s.contains("SET fetchable = true") || s.contains("SET fetchable = false") {
                    Some("fetchable")
                } else if s.contains("SET blocking_deps = x.n") {
                    Some("blocking_deps")
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            columns,
            [
                "fetchable",
                "fetchable",
                "fetchable",
                "fetchable",
                "blocking_deps",
                "blocking_deps"
            ],
            "two chunks, both fetchable passes first: {log:?}"
        );
        assert!(
            log[log.len() - 2].contains("SET status = 0")
                && log[log.len() - 1].contains("SET status = 1"),
            "the queue is settled after the counters: {log:?}"
        );
    }
}
