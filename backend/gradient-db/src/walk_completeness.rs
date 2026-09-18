/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation.unwalked_inputs`: how many direct inputs of a walked derivation have a
//! subtree that is not recorded. The walk prunes on `walked AND unwalked_inputs = 0`,
//! so the bit it trusts is maintained here the way `missing_references` is: seeded
//! when a record lands, rippled up as inputs complete, rippled back when a record is
//! dropped, recounted by the sweep. Every pass locks the rows it will write in id
//! order and touches `derivation` only, which is the first class in the lock order.
//!
//! # The seed is absolute, the ripples are relative
//!
//! [`seed_walk_completeness`] recounts the rows it is given against the inputs as
//! they stand; the ripples move a counter by the edges into a frontier. A relative
//! move must be driven by a TRANSITION or it drives a counter past zero, and a
//! negative counter never satisfies `= 0` again, so the seed reports which rows it
//! flipped and only those ripple.
//!
//! The pre-image cannot supply that transition on its own. The batch that walks a
//! derivation writes `walked = true` with the record, one statement before the seed,
//! so by the time the seed reads the row a freshly walked LEAF already reads
//! complete and would flip nothing, while its dependents counted it as a stub when
//! they were seeded and would wait for a count-down that never comes. The caller
//! therefore names which rows it just walked: those were incomplete before the batch
//! by definition, whatever the row says now.
//!
//! The mirror precondition holds for the seed's recount: a seeded row that loses
//! completeness is not rippled back, so only a caller whose rows cannot lose it may
//! seed. Ingest is such a caller, since a derivation's edges all land in the batch
//! that walks it; [`unwalk`] is the up-ripple for the one event that does take
//! completeness away.

use std::collections::BTreeMap;

use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};

use crate::readiness::ids;

/// The sweep's absolute recount: `incomplete` is every stub and, transitively,
/// everything above one, so a derivation's count is its direct inputs inside that
/// set. Only walked rows are rewritten; an unwalked row is incomplete by its own
/// bit and its counter is meaningless until the walk seeds it.
pub(crate) const RECOUNT_WALK_COMPLETENESS_SQL: &str = r#"
WITH RECURSIVE incomplete(id) AS (
    SELECT id FROM derivation WHERE NOT walked
    UNION
    SELECT e.derivation FROM derivation_dependency e JOIN incomplete i ON i.id = e.dependency),
counts AS (
    SELECT e.derivation AS id, count(*)::int AS n FROM derivation_dependency e
    JOIN incomplete i ON i.id = e.dependency GROUP BY e.derivation)
UPDATE derivation d SET unwalked_inputs = coalesce(c.n, 0)
FROM derivation w LEFT JOIN counts c ON c.id = w.id
WHERE d.id = w.id AND w.walked AND w.unwalked_inputs <> coalesce(c.n, 0)
"#;

crate::sql! {
    LOCK_WALK_ROWS = "SELECT id FROM derivation WHERE id = ANY($1::uuid[]) ORDER BY id FOR UPDATE",
        params = [DerivationIds(64)];

    /// Recount freshly recorded derivations against their inputs as they stand.
    /// `fresh` names a row this batch walked, which was incomplete before it
    /// whatever the pre-image says, so the caller ripples exactly what flipped.
    ///
    /// `Bulk` for the reason the readiness seed it mirrors is: a batch that reads
    /// the inputs of every value it was handed costs more than one row lookup per
    /// value, which is all the hot tier's ceiling allows for.
    SEED_UNWALKED_INPUTS = r#"
UPDATE derivation d SET unwalked_inputs = x.n
FROM (SELECT s.id,
             (d0.walked AND d0.unwalked_inputs = 0 AND NOT s.fresh) AS was_complete,
             (SELECT count(*) FROM derivation_dependency e
                JOIN derivation dep ON dep.id = e.dependency
               WHERE e.derivation = s.id
                 AND NOT (dep.walked AND dep.unwalked_inputs = 0))::int AS n
      FROM unnest($1::uuid[], $2::bool[]) AS s(id, fresh)
      JOIN derivation d0 ON d0.id = s.id) x
WHERE d.id = x.id
RETURNING d.id, x.was_complete, (d.walked AND d.unwalked_inputs = 0) AS complete
"#,
        params = [DerivationIds(64), Bools(false, 64)],
        tier = Bulk;

    DEPENDENT_COUNTS = "SELECT e.derivation AS id, count(*)::int AS n FROM derivation_dependency e WHERE e.dependency = ANY($1::uuid[]) GROUP BY e.derivation ORDER BY e.derivation",
        params = [DerivationIds(64)];

    COUNT_DOWN_UNWALKED = r#"
UPDATE derivation d SET unwalked_inputs = d.unwalked_inputs - c.n
FROM unnest($1::uuid[], $2::int[]) AS c(id, n)
WHERE d.id = c.id
RETURNING d.id, (d.walked AND d.unwalked_inputs = 0) AS complete
"#,
        params = [DerivationIds(64), Ints(1, 64)];

    COUNT_UP_UNWALKED = r#"
UPDATE derivation d SET unwalked_inputs = d.unwalked_inputs + c.n
FROM unnest($1::uuid[], $2::int[]) AS c(id, n)
WHERE d.id = c.id
RETURNING d.id, (d.walked AND d.unwalked_inputs = c.n) AS was_complete
"#,
        params = [DerivationIds(64), Ints(1, 64)];

    COMPLETE_AMONG = "SELECT id FROM derivation WHERE id = ANY($1::uuid[]) AND walked AND unwalked_inputs = 0 ORDER BY id FOR UPDATE",
        params = [DerivationIds(64)];

    UNWALK = "UPDATE derivation SET walked = false WHERE id = ANY($1)",
        params = [DerivationIds(64)];

    RECOUNT_WALK_COMPLETENESS = RECOUNT_WALK_COMPLETENESS_SQL,
        params = [],
        tier = Sweep,
        flags = [Walk];
}

fn derivation_ids(rows: &[QueryResult]) -> Vec<DerivationId> {
    rows.iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new)
        .collect()
}

fn flagged(rows: &[QueryResult], flag: &str) -> Vec<DerivationId> {
    rows.iter()
        .filter(|r| r.try_get::<bool>("", flag).unwrap_or(false))
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new)
        .collect()
}

/// Seed the counter of every derivation this batch recorded and ripple what
/// completed. `freshly_walked` are the rows the batch flipped to walked, `recounted`
/// the already-walked rows whose edges it grew; returns every derivation that became
/// complete, seeded or reached.
pub async fn seed_walk_completeness(
    txn: &DatabaseTransaction,
    freshly_walked: &[DerivationId],
    recounted: &[DerivationId],
) -> Result<Vec<DerivationId>, DbErr> {
    let mut seed: BTreeMap<DerivationId, bool> = recounted.iter().map(|d| (*d, false)).collect();
    seed.extend(freshly_walked.iter().map(|d| (*d, true)));
    if seed.is_empty() {
        return Ok(Vec::new());
    }

    let sorted: Vec<DerivationId> = seed.keys().copied().collect();
    let fresh: Vec<bool> = seed.values().copied().collect();
    txn.query_all_raw(LOCK_WALK_ROWS.bind([ids(&sorted)]))
        .await?;
    let rows = txn
        .query_all_raw(SEED_UNWALKED_INPUTS.bind([ids(&sorted), fresh.into()]))
        .await?;
    let flipped: Vec<DerivationId> = rows
        .iter()
        .filter(|r| {
            r.try_get::<bool>("", "complete").unwrap_or(false)
                && !r.try_get::<bool>("", "was_complete").unwrap_or(false)
        })
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new)
        .collect();
    let mut reached = flipped.clone();
    reached.extend(ripple(txn, &COUNT_DOWN_UNWALKED, "complete", flipped).await?);

    Ok(reached)
}

/// Drop the record of `derivations` and take the dependents of those that were
/// complete out of completeness with them.
pub async fn unwalk(txn: &DatabaseTransaction, derivations: &[DerivationId]) -> Result<(), DbErr> {
    if derivations.is_empty() {
        return Ok(());
    }

    let complete = derivation_ids(
        &txn.query_all_raw(COMPLETE_AMONG.bind([ids(derivations)]))
            .await?,
    );
    txn.execute_raw(UNWALK.bind([ids(derivations)])).await?;
    ripple(txn, &COUNT_UP_UNWALKED, "was_complete", complete).await?;

    Ok(())
}

/// One level per statement: the dependents of `frontier` are counted, locked in id
/// order and moved by their edge count; the ones `flag` names form the next level.
///
/// The counts are read separately so the update is a nested loop over a bound array
/// and takes its row locks in id order, which is the module doc's discipline; every
/// caller holds the rows through [`LOCK_WALK_ROWS`], so no edge can land between the
/// two halves.
async fn ripple(
    txn: &DatabaseTransaction,
    step: &crate::sql::Query,
    flag: &str,
    mut frontier: Vec<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    let mut reached = Vec::new();
    while !frontier.is_empty() {
        frontier.sort_unstable();
        frontier.dedup();
        let mut dependents: Vec<DerivationId> = Vec::new();
        let mut counts: Vec<i32> = Vec::new();
        for row in txn
            .query_all_raw(DEPENDENT_COUNTS.bind([ids(&frontier)]))
            .await?
        {
            dependents.push(DerivationId::new(row.try_get::<uuid::Uuid>("", "id")?));
            counts.push(row.try_get::<i32>("", "n")?);
        }
        if dependents.is_empty() {
            break;
        }

        txn.query_all_raw(LOCK_WALK_ROWS.bind([ids(&dependents)]))
            .await?;
        let rows = txn
            .query_all_raw(step.bind([ids(&dependents), counts.into()]))
            .await?;
        frontier = flagged(&rows, flag);
        reached.extend(frontier.iter().copied());
    }

    Ok(reached)
}

/// The sweep's recount, and the backfill after the column's migration.
pub async fn recount_walk_completeness<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
    let changed = walk
        .execute_raw(RECOUNT_WALK_COMPLETENESS.stmt())
        .await?
        .rows_affected();
    walk.commit().await?;

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};

    fn id_row(id: DerivationId, flags: &[(&str, bool)]) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::from([("id".to_owned(), Value::from(id.into_inner()))]);
        for (name, value) in flags {
            row.insert((*name).to_owned(), Value::from(*value));
        }

        row
    }

    fn count_row(id: DerivationId, n: i32) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("id".to_owned(), Value::from(id.into_inner())),
            ("n".to_owned(), Value::Int(Some(n))),
        ])
    }

    fn none() -> Vec<BTreeMap<String, Value>> {
        Vec::new()
    }

    /// A row that was complete before the seed and after it flipped nothing, so no
    /// dependent ever counted it and it must not count them down; only a row the
    /// seed flipped ripples, and the ripple stops at a level that flips nothing.
    #[tokio::test]
    async fn only_a_row_the_seed_flipped_ripples() {
        let settled = DerivationId::now_v7();
        let flipped = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([none()])
            .append_query_results([vec![
                id_row(settled, &[("was_complete", true), ("complete", true)]),
                id_row(flipped, &[("was_complete", false), ("complete", true)]),
            ]])
            .append_query_results([vec![count_row(parent, 1)]])
            .append_query_results([none()])
            .append_query_results([vec![id_row(parent, &[("complete", true)])]])
            .append_query_results([none()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let reached = seed_walk_completeness(&txn, &[], &[settled, flipped])
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(reached, vec![flipped, parent]);
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let counts: Vec<String> = log
            .iter()
            .filter(|s| {
                s.sql
                    .contains("count(*)::int AS n FROM derivation_dependency e WHERE e.dependency")
            })
            .map(|s| format!("{:?}", s.values))
            .collect();
        assert_eq!(counts.len(), 2, "one dependents lookup per level: {log:?}");
        assert!(
            counts[0].contains(&flipped.into_inner().to_string())
                && !counts[0].contains(&settled.into_inner().to_string()),
            "the settled row must not ripple: {}",
            counts[0]
        );
    }

    /// The batch writes `walked = true` one statement before the seed, so a freshly
    /// walked leaf reads complete on both sides of it. Its dependents counted it
    /// while it was a stub and are waiting for the count-down, so the caller's
    /// "this batch walked it" is the endpoint, not the row.
    #[tokio::test]
    async fn a_freshly_walked_leaf_ripples_though_the_row_reads_complete_throughout() {
        let leaf = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([none()])
            .append_query_results([vec![id_row(
                leaf,
                &[("was_complete", false), ("complete", true)],
            )]])
            .append_query_results([vec![count_row(parent, 1)]])
            .append_query_results([none()])
            .append_query_results([vec![id_row(parent, &[("complete", false)])]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let reached = seed_walk_completeness(&txn, &[leaf], &[]).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(reached, vec![leaf]);
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let seed = log
            .iter()
            .find(|s| s.sql.contains("SET unwalked_inputs = x.n"))
            .unwrap();
        assert!(
            format!("{:?}", seed.values).contains("Bool(Some(true))"),
            "the seed carries the batch's own endpoint: {:?}",
            seed.values
        );
        assert!(
            log.iter().any(|s| s
                .sql
                .contains("SET unwalked_inputs = d.unwalked_inputs - c.n")),
            "the leaf counts its dependents down: {log:?}"
        );
    }

    /// Rows are locked in id order before every update, and the seed runs on
    /// `derivation` only: no anchor row is touched, which is the class order the
    /// ingest and the un-walk both rely on.
    #[tokio::test]
    async fn every_update_follows_an_ordered_lock_on_derivation_rows_only() {
        let a = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([none()])
            .append_query_results([vec![id_row(
                a,
                &[("was_complete", false), ("complete", false)],
            )]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        seed_walk_completeness(&txn, &[a], &[]).await.unwrap();
        txn.commit().await.unwrap();

        let log = crate::pool::raw_statements(db.into_transaction_log());
        assert!(
            log[0]
                .sql
                .contains("FROM derivation WHERE id = ANY($1::uuid[]) ORDER BY id FOR UPDATE"),
            "the lock comes first: {log:?}"
        );
        assert!(
            log[1]
                .sql
                .contains("UPDATE derivation d SET unwalked_inputs"),
            "{log:?}"
        );
        assert!(
            log.iter().all(|s| !s.sql.contains("derivation_build")),
            "the pass never reaches an anchor: {log:?}"
        );
    }

    /// An un-walk takes the dependents of what WAS complete out of completeness
    /// with it, and stops at a dependent that was not complete to begin with.
    #[tokio::test]
    async fn an_unwalk_ripples_incompleteness_up_from_what_was_complete() {
        let gone = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![id_row(gone, &[])]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![count_row(parent, 2)]])
            .append_query_results([none()])
            .append_query_results([vec![id_row(parent, &[("was_complete", false)])]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        unwalk(&txn, &[gone]).await.unwrap();
        txn.commit().await.unwrap();

        let log = crate::pool::raw_statements(db.into_transaction_log());
        assert_eq!(
            log.iter()
                .filter(|s| s.sql.contains("unwalked_inputs + c.n"))
                .count(),
            1,
            "one count-up, then the ripple stops: {log:?}"
        );
        let up = log
            .iter()
            .find(|s| s.sql.contains("unwalked_inputs + c.n"))
            .unwrap();
        assert!(format!("{:?}", up.values).contains(&parent.into_inner().to_string()));
    }

    /// The recount is the sweep's backfill and repair: the incomplete set is every
    /// stub and everything above it, and only walked rows are rewritten.
    #[test]
    fn the_recount_walks_up_from_the_stubs() {
        let sql = RECOUNT_WALK_COMPLETENESS.text();
        assert!(sql.contains("WITH RECURSIVE incomplete(id) AS"), "{sql}");
        assert!(
            sql.contains("SELECT id FROM derivation WHERE NOT walked"),
            "{sql}"
        );
        assert!(
            sql.contains("JOIN incomplete i ON i.id = e.dependency"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "WHERE d.id = w.id AND w.walked AND w.unwalked_inputs <> coalesce(c.n, 0)"
            ),
            "{sql}"
        );
    }
}
