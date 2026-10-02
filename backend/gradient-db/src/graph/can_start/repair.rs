/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::predicates::fetchable_predicate;
use crate::graph::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::status::TransitionChange;

use super::fetchable::blocking_dependency_count;
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

static RECOUNT_FETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = x.f \
         FROM (SELECT p.derivation, p.fetchable AS old, {pred} AS f \
               FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])) x \
         WHERE db.derivation = x.derivation AND db.fetchable = x.old AND x.old <> x.f",
        pred = fetchable_predicate("p"),
    )
});

crate::sql_lazy! {
    RECOUNT_FETCHABLE_QUERY = || RECOUNT_FETCHABLE.as_str(),
        params = [DerivationIds(64)];
}

pub(super) static RECOUNT_BLOCKING_DEPS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET blocking_deps = x.n \
         FROM (SELECT p.derivation, p.blocking_deps AS old, {count} AS n \
               FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])) x \
         WHERE db.derivation = x.derivation AND db.blocking_deps = x.old AND x.old <> x.n",
        count = blocking_dependency_count("p", "dep.fetchable"),
    )
});

crate::sql_lazy! {
    RECOUNT_BLOCKING_DEPS_QUERY = || RECOUNT_BLOCKING_DEPS.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
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

async fn recount_fetchable(lock: &SharedBuildLock<'_>) -> Result<u64, DbErr> {
    recount(lock, &RECOUNT_FETCHABLE_QUERY).await
}

async fn recount_blocking_deps(lock: &SharedBuildLock<'_>) -> Result<u64, DbErr> {
    recount(lock, &RECOUNT_BLOCKING_DEPS_QUERY).await
}

async fn recount(lock: &SharedBuildLock<'_>, query: &crate::sql::Query) -> Result<u64, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(0);
    }

    Ok(lock
        .txn
        .execute_raw(query.bind([ids(&lock.derivations)]))
        .await?
        .rows_affected())
}

/// Every chunk is recounted before [`repair_can_start`] starts.
/// A counter computed from a stale `fetchable = true` would be too low and would promote.
pub async fn repair_fetchable<C>(db: &C, scope: &[DerivationId]) -> Result<u64, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut fetchable = 0u64;
    for chunk in scope.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_shared_builds(&txn, chunk).await?;
        fetchable += recount_fetchable(&lock).await?;
        txn.commit().await?;
    }

    Ok(fetchable)
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
    use crate::graph::can_start::fetchable::SEED_BLOCKING_DEPS;
    use crate::graph::can_start::test_rows::{drv, exec, norm, transition_row};
    use crate::pool::statements;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    #[test]
    fn the_seed_evaluates_the_predicate_while_the_recount_reads_the_column() {
        let seed = norm(&SEED_BLOCKING_DEPS);
        assert!(
            seed.contains(&norm(&fetchable_predicate("dep"))),
            "the seed must evaluate the predicate on its dependencies: {seed}"
        );
        assert!(
            !seed.contains("NOT (dep.fetchable)"),
            "the seed must not trust the column: {seed}"
        );

        let recount = norm(&RECOUNT_BLOCKING_DEPS);
        assert!(recount.contains("NOT (dep.fetchable)"), "{recount}");
        assert!(
            !recount.contains(&norm(&fetchable_predicate("dep"))),
            "the recount reads the column it just repaired: {recount}"
        );
    }

    #[test]
    fn the_recounts_are_compare_and_swaps_over_the_locked_chunk() {
        for sql in [norm(&RECOUNT_FETCHABLE), norm(&RECOUNT_BLOCKING_DEPS)] {
            assert!(
                sql.contains("FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])"),
                "the chunk is the whole bound: {sql}"
            );
            assert!(
                !sql.contains("WHERE p.status IN"),
                "a status scope would widen a recount past its lock: {sql}"
            );
        }

        assert!(
            norm(&RECOUNT_FETCHABLE).contains("db.fetchable = x.old AND x.old <> x.f"),
            "compare-and-swap plus drift filter"
        );
        assert!(
            norm(&RECOUNT_BLOCKING_DEPS).contains("db.blocking_deps = x.old AND x.old <> x.n"),
            "compare-and-swap plus drift filter"
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
            "the scope reads columns only; the recount evaluates the predicate: {sql}"
        );
    }

    #[tokio::test]
    async fn the_repair_locks_each_chunk_before_it_recounts_it() {
        let a = DerivationId::now_v7();
        let demoted = DerivationId::now_v7();
        let promoted = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv(a)]])
            .append_exec_results([exec(0), exec(2), exec(0), exec(3)])
            .append_query_results([vec![transition_row(demoted, 1, 0)]])
            .append_query_results([vec![drv(promoted)]])
            .into_connection();

        let scope = can_start_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let repaired = repair_can_start(&db, &scope).await.unwrap();

        assert_eq!((fetchable, repaired.blocking_deps), (2, 3));
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

        for (entry, recount) in [(1, "SET fetchable = x.f"), (2, "SET blocking_deps = x.n")] {
            let inside: Vec<&str> = raw[entry]
                .statements()
                .iter()
                .map(|s| s.sql.as_str())
                .collect();
            assert_eq!(inside.len(), 4, "BEGIN, lock, recount, COMMIT: {inside:?}");
            assert!(
                inside[1].contains("ORDER BY derivation FOR NO KEY UPDATE"),
                "{inside:?}"
            );
            assert!(inside[2].contains(recount), "{inside:?}");
        }

        let log = statements(raw);
        assert_eq!(log.len(), 7, "{log:?}");
        assert!(log[5].contains("SET status = 0"), "{log:?}");
        assert!(log[6].contains("SET status = 1"), "{log:?}");
    }

    #[tokio::test]
    async fn the_repair_finishes_fetchable_everywhere_before_it_recounts_a_counter() {
        let scope: Vec<BTreeMap<String, Value>> = (0..crate::IN_CHUNK_SIZE + 1)
            .map(|_| drv(DerivationId::now_v7()))
            .collect();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([scope])
            .append_exec_results(vec![exec(1); 8])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let scope = can_start_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let repaired = repair_can_start(&db, &scope).await.unwrap();

        assert_eq!(
            (fetchable, repaired.blocking_deps),
            (2, 2),
            "one row per chunk per pass"
        );

        let log = statements(db.into_transaction_log());
        let columns: Vec<&str> = log
            .iter()
            .filter_map(|s| {
                if s.contains("SET fetchable = x.f") {
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
            ["fetchable", "fetchable", "blocking_deps", "blocking_deps"],
            "two chunks, both fetchable passes first: {log:?}"
        );
    }
}
