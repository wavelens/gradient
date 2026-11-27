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
    SELECT s.id, count(*)::int AS n FROM incomplete i, LATERAL (SELECT e.derivation AS id FROM derivation_dependency e
     WHERE e.dependency = i.id AND e.kind IN (0, 2) OFFSET 0) s GROUP BY s.id)
UPDATE derivation d SET unwalked_inputs = coalesce(c.n, 0)
FROM derivation w LEFT JOIN counts c ON c.id = w.id
WHERE d.id = w.id AND w.walked AND w.unwalked_inputs <> coalesce(c.n, 0)
"#;

const UNWALKED_INPUTS_DRIFTED_SQL: &str = r#"
WITH marked_incomplete AS MATERIALIZED (
    SELECT id, walked, unwalked_inputs FROM derivation WHERE NOT walked OR unwalked_inputs <> 0),
counts AS (
    SELECT s.id, count(*)::int AS n FROM marked_incomplete i, LATERAL (SELECT e.derivation AS id FROM derivation_dependency e
     WHERE e.dependency = i.id AND e.kind IN (0, 2) OFFSET 0) s GROUP BY s.id)
SELECT 1 FROM marked_incomplete i FULL JOIN counts c ON c.id = i.id
WHERE i.id IS NULL OR (i.walked AND i.unwalked_inputs <> coalesce(c.n, 0))
LIMIT 1
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

    SELECT_DERIVATION_IDS_WITH_DRV_OR_INPUT_SOURCE_HASH_AMONG = "SELECT id FROM derivation WHERE hash = ANY($1::text[]) \
             UNION SELECT derivation FROM derivation_input_source WHERE hash = ANY($1::text[])",
        params = [CachedPathHashes(64)];

    UNWALKED_INPUTS_DRIFTED = UNWALKED_INPUTS_DRIFTED_SQL,
        params = [],
        tier = Sweep,
        flags = [Walk];

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

pub async fn mark_derivations_not_walked_for_store_path_hashes(
    txn: &DatabaseTransaction,
    hashes: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let mut owners = derivation_ids(
        &txn.query_all_raw(
            SELECT_DERIVATION_IDS_WITH_DRV_OR_INPUT_SOURCE_HASH_AMONG
                .bind([hashes.to_vec().into()]),
        )
        .await?,
    );
    owners.sort_unstable();
    unwalk(txn, &owners).await?;

    Ok(owners)
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
    crate::graph::walks::recount_if_drifted(
        db,
        &UNWALKED_INPUTS_DRIFTED,
        &RECOUNT_WALK_COMPLETENESS,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{mock_db, raw_statements};
    use sea_orm::{MockExecResult, Value};

    #[tokio::test]
    async fn freshly_walked_input_in_same_batch_counts_parent_input_incomplete_even_when_input_row_is_walked_with_zero_unwalked_inputs()
     {
        let [parent_path, input_path] =
            ["1", "2"].map(|c| format!("/nix/store/{}-seed.drv", c.repeat(32)));
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7())
            .derivation(&input_path)
            .derivation(&parent_path)
            .depends_on(&input_path)
            .into_database()
            .await;
        db.execute("UPDATE derivation SET walked = true, unwalked_inputs = 0")
            .await;
        let (parent, input) = (db.id_of(&parent_path).await, db.id_of(&input_path).await);
        let connection = db.connection();

        for (fresh, expected) in [(true, 1), (false, 0)] {
            connection
                .query_all_raw(
                    SEED_UNWALKED_INPUTS.bind([ids(&[parent, input]), vec![fresh, fresh].into()]),
                )
                .await
                .unwrap();
            let counted = db.derivation(&parent_path).await.unwalked_inputs;
            assert_eq!(counted, expected, "batch freshly walked: {fresh}");
        }
        db.finish().await;
    }

    #[test]
    fn every_edge_this_module_counts_is_a_build_edge() {
        let statements = crate::sql::registry()
            .filter(|q| q.file.ends_with("walk_completeness.rs"))
            .map(|q| (q.name, q.text()))
            .chain([("RIPPLE_UNWALKED_INPUTS_FN", ripple_fn().into())]);
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

    fn ripple_fn() -> &'static str {
        gradient_migration::function_sql("ripple_unwalked_inputs")
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
        let db = mock_db()
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
        let log = raw_statements(db.into_transaction_log());
        let ripples: Vec<String> = log
            .iter()
            .filter(|s| {
                s.sql
                    .contains("FROM ripple_unwalked_inputs($1::uuid[], $2::bool)")
            })
            .map(|s| format!("{:?}", s.values))
            .collect();
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
        let db = mock_db()
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
        let log = raw_statements(db.into_transaction_log());
        let seed = log
            .iter()
            .find(|s| s.sql.contains("SET unwalked_inputs = x.n"))
            .unwrap();
        assert!(
            format!("{:?}", seed.values).contains("Bool(Some(true))"),
            "the seed carries the batch's own endpoint: {:?}",
            seed.values
        );
    }

    #[tokio::test]
    async fn an_unwalk_ripples_incompleteness_up_from_what_was_complete() {
        let gone = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = mock_db()
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

        let log = raw_statements(db.into_transaction_log());
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

    #[tokio::test]
    async fn test_recount_walk_completeness_rewrites_drifted_unwalked_input_counts_from_unwalked_derivations_build_edges_and_second_run_writes_nothing()
     {
        let path = |c: &str| format!("/nix/store/{}-recount.drv", c.repeat(32));
        let [stub, above_stub, runtime_only, above_incomplete, settled] = ["1", "2", "3", "4", "5"];
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7())
            .derivation(&path(stub))
            .derivation(&path(above_stub))
            .walked()
            .depends_on(&path(stub))
            .derivation(&path(runtime_only))
            .walked()
            .runtime_depends_on(&path(stub))
            .derivation(&path(above_incomplete))
            .walked()
            .depends_on(&path(above_stub))
            .derivation(&path(settled))
            .walked()
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE derivation SET unwalked_inputs = 5; \
             UPDATE derivation SET unwalked_inputs = 0 WHERE id = '{}'",
            db.id_of(&path(settled)).await
        ))
        .await;

        let rewritten = recount_walk_completeness(&db.connection()).await.unwrap();
        let mut counts = Vec::new();
        for key in [stub, above_stub, runtime_only, above_incomplete, settled] {
            counts.push(db.derivation(&path(key)).await.unwalked_inputs);
        }
        let rewritten_again = recount_walk_completeness(&db.connection()).await.unwrap();
        db.finish().await;

        assert_eq!((rewritten, rewritten_again), (3, 0));
        assert_eq!(counts, [5, 1, 0, 1, 0]);
    }
}
