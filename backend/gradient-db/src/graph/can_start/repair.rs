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

/// The pending shared builds and their direct dependencies, every row whose `fetchable` a
/// gate can read this pass, and every row whose stored flag contradicts a column it
/// carries, which no pending shared build need be near. [`can_start_scope`] materialises
/// it, so the lock and the recounts name one frozen list rather than a subquery each
/// statement re-evaluates against its own snapshot. The two contradiction arms are
/// partial-index scans: `idx-derivation_build-fetchable-incomplete` and
/// `idx-derivation_build-open`.
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

/// What the can-start half of the consistency check repaired.
#[derive(Debug, Default)]
pub struct Repaired {
    pub blocking_deps: u64,
    pub promoted: Vec<TransitionChange>,
    pub unpromoted: Vec<TransitionChange>,
}

/// Materialise [`repair_scope`] once, so every chunk the two repairs lock and
/// recount comes from one snapshot instead of a subquery each statement re-evaluates
/// against its own. Its length is the sweep's `repair_scope`: a measurement, not a
/// violation, since the select is unbounded and each chunk takes `FOR NO KEY UPDATE` on rows
/// every live graph writer also locks.
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

/// Recount `fetchable` for the locked chunk and write the rows that disagree.
async fn recount_fetchable(lock: &SharedBuildLock<'_>) -> Result<u64, DbErr> {
    recount(lock, &RECOUNT_FETCHABLE_QUERY).await
}

/// Recount `blocking_deps` for the locked chunk and write the rows that disagree.
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

/// Recount `fetchable` over `scope` and write only what differs. One transaction
/// per chunk, each taking [`lock_shared_builds`] before it recounts, so no recount writes a
/// row it did not lock and a cancelled sweep loses one chunk rather than every repair.
///
/// It finishes the whole scope before [`repair_can_start`] starts, and before the
/// sweep's need recount: a counter computed from a stale `fetchable = true` is too
/// low and promotes, and a walk that reads one stops at a settled shared build that is not.
/// The compare-and-swap on the pre-image (`db.fetchable = x.old AND x.old <> x.f`)
/// stays. Under the lock it compares the row to itself, but a future caller that loses
/// the lock degrades to a skipped row rather than to an unconditional overwrite of a
/// value nothing else re-derives, and `old <> new` is the drift filter behind the
/// returned count.
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

/// Recount `blocking_deps` over `scope`, chunked and locked as [`repair_fetchable`]
/// is, then settle the queue against the gates on the caller's handle. Both settling
/// statements re-check the target row's own `status` and `blocking_deps`, which
/// EvalPlanQual does re-evaluate, so they are exactly as safe as the live promotion
/// path and no safer: the gate's three `EXISTS` subqueries are not re-evaluated, so a
/// promote can still fire on a `.drv` retired during a lock wait. The un-promote goes
/// first, and the two cannot both move a row because
/// [`crate::graph::predicates::gates_predicate`] never reads `status`.
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

    /// The seed is the first reader of a dependency's can-start state and executes before any
    /// sweep could have corrected a stale flag, so it must EVALUATE the predicate on
    /// each dependency rather than read the column. The recount, which takes place right
    /// after its own repair of that column, reads the column.
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

    /// Both recounts are bounded by the chunk their lock named, never by a subquery a
    /// second statement would re-evaluate against its own snapshot, and both write as a
    /// compare-and-swap on the pre-image rather than an unconditional overwrite.
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

    /// The scope is the pending shared builds and one edge past them, plus every row whose
    /// flag contradicts a column it carries, and it is SELECTed once rather than left
    /// as a subquery: an inline scope is re-evaluated per statement, so a row entering
    /// it after the lock would be written unlocked, which is the very ABA the lock
    /// exists to stop. The contradiction arms are what the complete-closure recount leaves
    /// behind two hops below anything pending, and both are column-only tests so a
    /// partial index answers each.
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

    /// Every recount must be preceded by an ordered lock in its own transaction, so no
    /// row is written from a snapshot a concurrent flip could have moved under it.
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

    /// Every chunk's `fetchable` recount lands before the first counter recount: a
    /// counter computed from a `fetchable = true` a later chunk was about to correct
    /// comes out too LOW, and too low promotes and dispatches. The two are separate
    /// passes over one frozen scope so the sweep can put its need recount between
    /// them.
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
