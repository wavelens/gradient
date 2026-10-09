/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `fresh` is overriding both sides of the seed.
//! A parent and its inputs can share a batch, and the upsert already wrote those inputs complete.
//! Counting them as read would drive the parent below zero, where `= 0` is never true again.
//!
//! Every read is `kind IN (0, 2)`, because a runtime dependency is not a walk input.
//! A runtime edge is landing without a seed of its own and would break the counter the same way.

use std::collections::BTreeMap;

use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};

use crate::graph::can_start::ids;

pub(crate) const RECOUNT_WALK_COMPLETENESS_SQL: &str = r#"
WITH RECURSIVE incomplete(id) AS (
    SELECT id FROM derivation WHERE NOT walked
    UNION
    SELECT s.id FROM incomplete i, LATERAL (SELECT e.derivation AS id FROM derivation_dependency e
     WHERE e.dependency = i.id AND e.kind IN (0, 2) OFFSET 0) s),
counts AS (
    SELECT e.derivation AS id, count(*)::int AS n FROM derivation_dependency e
    JOIN incomplete i ON i.id = e.dependency
    WHERE e.kind IN (0, 2) GROUP BY e.derivation)
UPDATE derivation d SET unwalked_inputs = coalesce(c.n, 0)
FROM derivation w LEFT JOIN counts c ON c.id = w.id
WHERE d.id = w.id AND w.walked AND w.unwalked_inputs <> coalesce(c.n, 0)
"#;

crate::sql! {
    LOCK_WALK_ROWS = "SELECT id FROM derivation WHERE id = ANY($1::uuid[]) ORDER BY id FOR NO KEY UPDATE",
        params = [DerivationIds(64)];

    SEED_UNWALKED_INPUTS = r#"
UPDATE derivation d SET unwalked_inputs = x.n
FROM (SELECT s.id,
             (d0.walked AND d0.unwalked_inputs = 0 AND NOT s.fresh) AS was_complete,
             (SELECT count(*) FROM derivation_dependency e
                JOIN derivation dep ON dep.id = e.dependency
                LEFT JOIN unnest($1::uuid[], $2::bool[]) AS f(id, fresh)
                  ON f.id = e.dependency
               WHERE e.derivation = s.id AND e.kind IN (0, 2)
                 AND NOT (dep.walked AND dep.unwalked_inputs = 0
                          AND NOT coalesce(f.fresh, false)))::int AS n
      FROM unnest($1::uuid[], $2::bool[]) AS s(id, fresh)
      JOIN derivation d0 ON d0.id = s.id) x
WHERE d.id = x.id
RETURNING d.id, x.was_complete, (d.walked AND d.unwalked_inputs = 0) AS complete
"#,
        params = [DerivationIds(64), Bools(false, 64)],
        tier = Bulk;

    RIPPLE_UNWALKED_INPUTS = "SELECT id FROM ripple_unwalked_inputs($1::uuid[], $2::bool) AS r(id)",
        params = [DerivationIds(64), Bool(true)],
        tier = Bulk;

    COMPLETE_AMONG = "SELECT id FROM derivation WHERE id = ANY($1::uuid[]) AND walked AND unwalked_inputs = 0 ORDER BY id FOR NO KEY UPDATE",
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
    reached.extend(ripple(txn, flipped, true).await?);

    Ok(reached)
}

pub async fn unwalk(txn: &DatabaseTransaction, derivations: &[DerivationId]) -> Result<(), DbErr> {
    if derivations.is_empty() {
        return Ok(());
    }

    let complete = derivation_ids(
        &txn.query_all_raw(COMPLETE_AMONG.bind([ids(derivations)]))
            .await?,
    );
    txn.execute_raw(UNWALK.bind([ids(derivations)])).await?;
    ripple(txn, complete, false).await?;

    Ok(())
}

async fn ripple(
    txn: &DatabaseTransaction,
    mut frontier: Vec<DerivationId>,
    down: bool,
) -> Result<Vec<DerivationId>, DbErr> {
    if frontier.is_empty() {
        return Ok(Vec::new());
    }
    frontier.sort_unstable();
    frontier.dedup();

    Ok(derivation_ids(
        &txn.query_all_raw(RIPPLE_UNWALKED_INPUTS.bind([ids(&frontier), down.into()]))
            .await?,
    ))
}

pub async fn recount_walk_completeness<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &RECOUNT_WALK_COMPLETENESS).await?;
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

    #[test]
    fn a_fresh_input_is_counted_as_incomplete_by_its_parent() {
        let sql = SEED_UNWALKED_INPUTS.text();
        assert!(
            sql.contains("LEFT JOIN unnest($1::uuid[], $2::bool[]) AS f(id, fresh)")
                && sql.contains("ON f.id = e.dependency"),
            "the count must read the batch's fresh set on the dependency side: {sql}"
        );
        assert!(
            sql.contains("AND NOT coalesce(f.fresh, false)"),
            "a fresh input must read incomplete however the row reads now: {sql}"
        );
    }

    #[test]
    fn every_edge_this_module_counts_is_a_build_edge() {
        let statements = crate::sql::registry()
            .filter(|q| q.file.ends_with("walk_completeness.rs"))
            .map(|q| (q.name, q.text()))
            .chain([("RIPPLE_UNWALKED_INPUTS_FN", RIPPLE_FN.into())]);
        let mut edges = 0;
        for (name, sql) in statements {
            assert_eq!(
                sql.matches("derivation_dependency").count(),
                sql.matches("kind IN (0, 2)").count(),
                "{name} reads an edge with no build-edge filter: {sql}",
            );
            edges += sql.matches("derivation_dependency").count();
        }
        assert!(
            edges > 1,
            "the registry did not reach this module's statements"
        );
    }

    const RIPPLE_FN: &str =
        gradient_migration::m20261001_000001_plain_concept_names::RIPPLE_UNWALKED_INPUTS_FN;

    #[test]
    fn the_ripple_locks_each_level_in_id_order_before_it_writes() {
        let lock = RIPPLE_FN
            .find("FROM derivation WHERE id = ANY(parents) ORDER BY id FOR NO KEY UPDATE")
            .expect("the ordered lock");
        let write = RIPPLE_FN
            .find("SET unwalked_inputs = d.unwalked_inputs")
            .expect("the write");
        assert!(lock < write, "{RIPPLE_FN}");
        assert!(
            RIPPLE_FN.contains(
                "(d.walked AND d.unwalked_inputs = CASE WHEN down THEN 0 ELSE c.n END) AS flipped"
            ),
            "{RIPPLE_FN}"
        );
        assert!(
            RIPPLE_FN.contains("SELECT unnest(wave)")
                && RIPPLE_FN.contains("FROM moved WHERE flipped"),
            "only the rows that flipped form the next level and the result: {RIPPLE_FN}"
        );
    }

    fn id_row(id: DerivationId, flags: &[(&str, bool)]) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::from([("id".to_owned(), Value::from(id.into_inner()))]);
        for (name, value) in flags {
            row.insert((*name).to_owned(), Value::from(*value));
        }

        row
    }

    fn none() -> Vec<BTreeMap<String, Value>> {
        Vec::new()
    }

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
            .append_query_results([vec![id_row(parent, &[])]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let reached = seed_walk_completeness(&txn, &[], &[settled, flipped])
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(reached, vec![flipped, parent]);
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let ripples: Vec<String> = log
            .iter()
            .filter(|s| {
                s.sql
                    .contains("FROM ripple_unwalked_inputs($1::uuid[], $2::bool)")
            })
            .map(|s| format!("{:?}", s.values))
            .collect();
        assert_eq!(ripples.len(), 1, "one call runs every level: {log:?}");
        assert!(
            ripples[0].contains(&flipped.into_inner().to_string())
                && !ripples[0].contains(&settled.into_inner().to_string())
                && ripples[0].contains("Bool(Some(true))"),
            "the settled row must not ripple, and a seed ripples down: {}",
            ripples[0]
        );
    }

    #[tokio::test]
    async fn a_freshly_walked_leaf_ripples_though_the_row_reads_complete_throughout() {
        let leaf = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([none()])
            .append_query_results([vec![id_row(
                leaf,
                &[("was_complete", false), ("complete", true)],
            )]])
            .append_query_results([none()])
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
            log.iter()
                .any(|s| s.sql.contains("FROM ripple_unwalked_inputs(")),
            "the leaf counts its parents down: {log:?}"
        );
    }

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
            log[0].sql.contains(
                "FROM derivation WHERE id = ANY($1::uuid[]) ORDER BY id FOR NO KEY UPDATE"
            ),
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
            "the pass never reaches a shared build: {log:?}"
        );
    }

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
            .append_query_results([vec![id_row(parent, &[])]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        unwalk(&txn, &[gone]).await.unwrap();
        txn.commit().await.unwrap();

        let log = crate::pool::raw_statements(db.into_transaction_log());
        let up = log
            .iter()
            .find(|s| s.sql.contains("FROM ripple_unwalked_inputs("))
            .expect("the un-walk ripples up from what was complete");
        let values = format!("{:?}", up.values);
        assert!(
            values.contains(&gone.into_inner().to_string()) && values.contains("Bool(Some(false))"),
            "{values}"
        );
    }

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
        // A joined step is merge-joining every edge once per level, ~190 s on prod.
        // The fenced probe is reading only the edges into the frontier.
        assert!(
            sql.contains("SELECT s.id FROM incomplete i, LATERAL (SELECT e.derivation AS id")
                && sql.contains("WHERE e.dependency = i.id AND e.kind IN (0, 2) OFFSET 0) s"),
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
