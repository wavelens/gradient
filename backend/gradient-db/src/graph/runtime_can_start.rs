/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The seed cannot read the transition off the row it is writing.
//! The commit making an output present wrote `cached_path` one statement earlier.
//! `freshly_present` is the caller's endpoint for that transition.
//! A re-push of an already backed path must not count as fresh.

use std::collections::BTreeMap;

use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};

use crate::graph::can_start::{ids, lock_shared_builds};
use crate::graph::predicates::{present_predicate, shared_build_complete_predicate};

fn seed_sql() -> String {
    format!(
        "UPDATE derivation_build db SET missing_runtime_deps = x.n \
         FROM (SELECT b.id, ({was_complete} AND NOT s.fresh) AS was_complete, \
                      (SELECT count(*) FROM derivation_dependency e \
                         JOIN derivation_build dep ON dep.derivation = e.dependency \
                        WHERE e.derivation = b.derivation AND e.kind IN (1, 2) \
                          AND NOT {dep_complete})::int AS n \
               FROM unnest($1::uuid[], $2::bool[]) AS s(derivation, fresh) \
               JOIN derivation_build b ON b.derivation = s.derivation) x \
         WHERE db.id = x.id \
         RETURNING db.derivation, x.was_complete, {complete} AS complete",
        was_complete = shared_build_complete_predicate("b"),
        dep_complete = shared_build_complete_predicate("dep"),
        complete = shared_build_complete_predicate("db"),
    )
}

fn complete_among_sql() -> String {
    format!(
        "SELECT db.derivation FROM derivation_build db \
         WHERE db.derivation = ANY($1::uuid[]) AND {complete} \
         ORDER BY db.derivation FOR NO KEY UPDATE",
        complete = shared_build_complete_predicate("db"),
    )
}

fn recount_sql() -> String {
    format!(
        "WITH RECURSIVE incomplete(derivation) AS (\
             SELECT db.derivation FROM derivation_build db WHERE NOT {present} \
             UNION \
             SELECT e.derivation FROM derivation_dependency e \
             JOIN incomplete u ON u.derivation = e.dependency WHERE e.kind IN (1, 2)), \
         counts AS (\
             SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e \
             JOIN incomplete u ON u.derivation = e.dependency WHERE e.kind IN (1, 2) \
             GROUP BY e.derivation) \
         UPDATE derivation_build db SET missing_runtime_deps = coalesce(c.n, 0) \
         FROM derivation_build b LEFT JOIN counts c ON c.derivation = b.derivation \
         WHERE db.id = b.id AND b.missing_runtime_deps <> coalesce(c.n, 0)",
        present = present_predicate("db"),
    )
}

crate::sql_fn! {
    SEED_MISSING_RUNTIME_DEPS = seed_sql,
        params = [DerivationIds(64), Bools(false, 64)],
        tier = Bulk;

    CLOSURE_COMPLETE_AMONG = complete_among_sql,
        params = [DerivationIds(64)];

    RECOUNT_MISSING_RUNTIME_DEPS = recount_sql,
        params = [],
        tier = Sweep,
        flags = [Walk];
}

fn complete_output_hashes_sql() -> String {
    format!(
        "SELECT cp.hash FROM cached_path cp \
         WHERE cp.hash = ANY($1) AND cp.file_hash IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM derivation_output o \
                           JOIN derivation_build db ON db.derivation = o.derivation \
                           WHERE o.hash = cp.hash AND NOT {complete})",
        complete = shared_build_complete_predicate("db"),
    )
}

crate::sql_fn! {
    COMPLETE_OUTPUT_HASHES = complete_output_hashes_sql,
        params = [CachedPathHashes(64)],
        tier = Bulk;
}

pub async fn complete_output_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<String>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    Ok(db
        .query_all_raw(COMPLETE_OUTPUT_HASHES.bind([hashes.to_vec().into()]))
        .await?
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect())
}

fn lock_cached_paths_sql() -> String {
    let (with, filter) = crate::graph::shared_build_guard::producer_filter("$1");
    format!(
        "WITH {with} SELECT 1 FROM cached_path \
         WHERE hash = ANY($1) AND {filter} ORDER BY hash FOR NO KEY UPDATE"
    )
}

fn reset_uncached_producers_sql() -> String {
    format!(
        "UPDATE derivation_build db \
         SET status = {created}, substituted = false, attempt = 0, \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) \
           AND db.status IN ({terminal_success}) AND NOT db.fetchable \
         RETURNING db.derivation, old.status AS from_status, db.status AS to_status",
        created = crate::sql::status::build(gradient_entity::build::BuildStatus::Created),
        terminal_success =
            crate::sql::status::build_in(&gradient_entity::build::BuildStatus::TERMINAL_SUCCESS),
    )
}

crate::sql_fn! {
    RESET_UNCACHED_PRODUCERS = reset_uncached_producers_sql,
        params = [DerivationIds(64)];
}

crate::sql_fn! {
    LOCK_CACHED_PATHS = lock_cached_paths_sql,
        params = [CachedPathHashes(64)];
}

crate::sql! {
    DELETE_CACHED_PATHS = "DELETE FROM cached_path cp WHERE cp.hash = ANY($1) RETURNING cp.hash",
        params = [CachedPathHashes(64)];

    SET_OUTPUTS_UNCACHED = "UPDATE derivation_output SET is_cached = false WHERE is_cached AND hash = ANY($1)",
        params = [CachedPathHashes(64)];

    RIPPLE_MISSING_RUNTIME_DEPS = "SELECT derivation FROM ripple_missing_runtime_deps($1::uuid[], $2::bool) AS r(derivation)",
        params = [DerivationIds(64), Bool(true)],
        tier = Bulk;
}

#[derive(Debug, Default)]
pub struct Seeded {
    pub complete: Vec<DerivationId>,
    pub incomplete: Vec<DerivationId>,
}

fn derivation_ids(rows: &[QueryResult]) -> Vec<DerivationId> {
    rows.iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

pub async fn seed_runtime_deps(
    txn: &DatabaseTransaction,
    freshly_present: &[DerivationId],
    recounted: &[DerivationId],
) -> Result<Seeded, DbErr> {
    let mut seed: BTreeMap<DerivationId, bool> = recounted.iter().map(|d| (*d, false)).collect();
    seed.extend(freshly_present.iter().map(|d| (*d, true)));
    if seed.is_empty() {
        return Ok(Seeded::default());
    }

    let sorted: Vec<DerivationId> = seed.keys().copied().collect();
    let fresh: Vec<bool> = seed.values().copied().collect();
    let _lock = crate::graph::can_start::lock_seed_shared_builds(txn, &sorted).await?;
    let rows = txn
        .query_all_raw(SEED_MISSING_RUNTIME_DEPS.bind([ids(&sorted), fresh.into()]))
        .await?;

    let mut moved = Seeded::default();
    for row in &rows {
        let (Ok(derivation), Ok(was_complete), Ok(complete)) = (
            row.try_get::<uuid::Uuid>("", "derivation"),
            row.try_get::<bool>("", "was_complete"),
            row.try_get::<bool>("", "complete"),
        ) else {
            continue;
        };

        match (was_complete, complete) {
            (false, true) => moved.complete.push(DerivationId::new(derivation)),
            (true, false) => moved.incomplete.push(DerivationId::new(derivation)),
            _ => {}
        }
    }

    let gained = moved.complete.clone();
    let lost = moved.incomplete.clone();
    moved
        .complete
        .extend(ripple_shared_builds_complete(txn, gained).await?);
    moved
        .incomplete
        .extend(ripple_shared_builds_incomplete(txn, lost).await?);

    Ok(moved)
}

pub async fn ripple_shared_builds_complete(
    txn: &DatabaseTransaction,
    frontier: Vec<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    ripple(txn, frontier, true).await
}

pub async fn ripple_shared_builds_incomplete(
    txn: &DatabaseTransaction,
    frontier: Vec<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    ripple(txn, frontier, false).await
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
        &txn.query_all_raw(RIPPLE_MISSING_RUNTIME_DEPS.bind([ids(&frontier), down.into()]))
            .await?,
    ))
}

/// A transaction writing `derivation_build` before a retire must take these locks first.
/// Every other writer is following the class order `cached_path`, then `derivation_build`.
pub async fn lock_cached_paths(txn: &DatabaseTransaction, hashes: &[String]) -> Result<(), DbErr> {
    if hashes.is_empty() {
        return Ok(());
    }

    txn.execute_raw(LOCK_CACHED_PATHS.bind([hashes.to_vec().into()]))
        .await?;

    Ok(())
}

pub async fn complete_among(
    txn: &DatabaseTransaction,
    derivations: &[DerivationId],
) -> Result<Vec<DerivationId>, DbErr> {
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    Ok(derivation_ids(
        &txn.query_all_raw(CLOSURE_COMPLETE_AMONG.bind([ids(derivations)]))
            .await?,
    ))
}

#[derive(Debug, Default)]
pub struct Retired {
    pub deleted: Vec<String>,
    pub incomplete: Vec<DerivationId>,
    pub transitions: Vec<crate::status::TransitionChange>,
}

pub async fn retire_outputs(
    txn: &DatabaseTransaction,
    hashes: &[String],
) -> Result<Retired, DbErr> {
    if hashes.is_empty() {
        return Ok(Retired::default());
    }

    txn.execute_raw(LOCK_CACHED_PATHS.bind([hashes.to_vec().into()]))
        .await?;

    let gone = crate::graph::reachability::producers_of_hashes(txn, hashes).await?;
    let was_complete = complete_among(txn, &gone).await?;

    let deleted: Vec<String> = txn
        .query_all_raw(DELETE_CACHED_PATHS.bind([hashes.to_vec().into()]))
        .await?
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect();

    if !deleted.is_empty() {
        txn.execute_raw(SET_OUTPUTS_UNCACHED.bind([deleted.clone().into()]))
            .await?;
    }

    let mut incomplete = was_complete.clone();
    incomplete.extend(ripple_shared_builds_incomplete(txn, was_complete).await?);
    let transitions = retire_shared_builds(txn, &gone, &incomplete, hashes).await?;

    Ok(Retired {
        deleted,
        incomplete,
        transitions,
    })
}

async fn retire_shared_builds(
    txn: &DatabaseTransaction,
    gone: &[DerivationId],
    incomplete: &[DerivationId],
    hashes: &[String],
) -> Result<Vec<crate::status::TransitionChange>, DbErr> {
    let mut scope = gone.to_vec();
    scope.extend_from_slice(incomplete);
    scope.sort_unstable();
    scope.dedup();

    let lock = lock_shared_builds(txn, &scope).await?;
    let mut transitions = crate::graph::can_start::lost_fetchability(&lock).await?;

    if !gone.is_empty() {
        let reset = crate::graph::promotion::returned_transitions(
            txn.query_all_raw(RESET_UNCACHED_PRODUCERS.bind([ids(gone)]))
                .await?,
        );
        if !reset.is_empty() {
            let thawed: Vec<DerivationId> = reset.iter().map(|c| c.derivation).collect();
            let thawed_lock =
                crate::graph::can_start::lock_seed_shared_builds(txn, &thawed).await?;
            crate::graph::can_start::seed_blocking_deps(&thawed_lock).await?;
        }

        transitions.extend(reset);
    }

    transitions.extend(crate::graph::can_start::unpromote_drv_owners(txn, hashes).await?);

    Ok(transitions)
}

pub async fn recount_missing_runtime_deps<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &RECOUNT_MISSING_RUNTIME_DEPS).await?;
    let changed = walk
        .execute_raw(RECOUNT_MISSING_RUNTIME_DEPS.stmt())
        .await?
        .rows_affected();
    walk.commit().await?;

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};

    fn seed_row(id: DerivationId, was_complete: bool, complete: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("was_complete".to_owned(), Value::from(was_complete)),
            ("complete".to_owned(), Value::from(complete)),
        ])
    }

    fn flag_row(id: DerivationId, flag: &str, value: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            (flag.to_owned(), Value::from(value)),
        ])
    }

    fn none() -> Vec<BTreeMap<String, Value>> {
        Vec::new()
    }

    const RIPPLE_FN: &str =
        gradient_migration::m20261001_000001_plain_concept_names::RIPPLE_MISSING_RUNTIME_DEPS_FN;

    #[test]
    fn the_retire_holds_its_producers_keys_before_it_reads_their_completeness() {
        let lock = LOCK_CACHED_PATHS.text();
        assert!(lock.contains("pg_advisory_xact_lock(643, k)"), "{lock}");
        assert!(
            lock.contains("FROM derivation_output o WHERE o.hash = ANY($1)"),
            "{lock}"
        );
        assert!(!lock.contains("derivation_dependency"), "{lock}");
        assert!(lock.contains("ORDER BY hash FOR NO KEY UPDATE"), "{lock}");

        let complete = CLOSURE_COMPLETE_AMONG.text();
        assert!(!complete.contains("pg_advisory"), "{complete}");
        assert!(
            complete.contains("ORDER BY db.derivation FOR NO KEY UPDATE"),
            "{complete}"
        );
    }

    #[tokio::test]
    async fn only_a_shared_build_the_seed_flipped_ripples() {
        let settled = DerivationId::now_v7();
        let flipped = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![
                seed_row(settled, true, true),
                seed_row(flipped, false, true),
            ]])
            .append_query_results([vec![flag_row(parent, "complete", true)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let moved = seed_runtime_deps(&txn, &[], &[settled, flipped])
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(moved.complete, vec![flipped, parent]);
        assert!(moved.incomplete.is_empty());
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let ripples: Vec<String> = log
            .iter()
            .filter(|s| {
                s.sql
                    .contains("FROM ripple_missing_runtime_deps($1::uuid[], $2::bool)")
            })
            .map(|s| format!("{:?}", s.values))
            .collect();
        assert_eq!(ripples.len(), 1, "one call runs every level: {log:?}");
        let guard = log
            .iter()
            .position(|s| s.sql.contains("pg_advisory_xact_lock_shared(643, k)"))
            .expect("the seed counts under its dependencies' shared keys");
        let seed = log
            .iter()
            .position(|s| s.sql.contains("SET missing_runtime_deps = x.n"))
            .expect("the seed");
        assert!(
            guard < seed,
            "the keys are held before the count reads: {log:?}"
        );
        assert!(
            ripples[0].contains(&flipped.into_inner().to_string())
                && !ripples[0].contains(&settled.into_inner().to_string())
                && ripples[0].contains("Bool(Some(true))"),
            "the settled row must not ripple, and a seed ripples down: {}",
            ripples[0]
        );
    }

    #[tokio::test]
    async fn a_freshly_present_shared_build_ripples_though_the_row_reads_complete_throughout() {
        let landed = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![seed_row(landed, false, true)]])
            .append_query_results([none()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let moved = seed_runtime_deps(&txn, &[landed], &[]).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(moved.complete, vec![landed]);
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let seed = log
            .iter()
            .find(|s| s.sql.contains("SET missing_runtime_deps = x.n"))
            .expect("the seed runs");
        assert!(
            seed.sql.contains("AND NOT s.fresh) AS was_complete"),
            "the caller's endpoint has to beat the row: {}",
            seed.sql
        );
    }

    #[test]
    fn the_ripple_flips_on_the_module_predicates() {
        assert!(
            RIPPLE_FN.contains(&format!(
                "CASE WHEN down THEN {}",
                shared_build_complete_predicate("db")
            )),
            "{RIPPLE_FN}"
        );
        assert!(
            RIPPLE_FN.contains(&format!(
                "ELSE (db.missing_runtime_deps = c.n AND {})",
                crate::graph::predicates::present_value("db")
            )),
            "{RIPPLE_FN}"
        );
        assert_eq!(
            RIPPLE_FN.matches("derivation_dependency").count(),
            RIPPLE_FN.matches("kind IN (1, 2)").count(),
            "every edge the ripple counts is a runtime dependency: {RIPPLE_FN}"
        );
    }

    #[test]
    fn every_level_holds_its_keys_and_ordered_rows_before_it_writes() {
        let keys = RIPPLE_FN
            .find("PERFORM pg_advisory_xact_lock(643, k)")
            .expect("the keys");
        let rows = RIPPLE_FN
            .find("WHERE derivation = ANY(parents) ORDER BY derivation FOR NO KEY UPDATE")
            .expect("the ordered row lock");
        let write = RIPPLE_FN
            .find("SET missing_runtime_deps = db.missing_runtime_deps")
            .expect("the write");
        assert!(keys < rows && rows < write, "{RIPPLE_FN}");
    }

    #[tokio::test]
    async fn a_retire_ripples_incompleteness_up_from_what_was_complete() {
        let gone = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![flag_row(parent, "was_complete", true)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let reached = ripple_shared_builds_incomplete(&txn, vec![gone])
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(reached, vec![parent]);
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let up = log
            .iter()
            .find(|s| s.sql.contains("FROM ripple_missing_runtime_deps("))
            .expect("the retire ripples up");
        let values = format!("{:?}", up.values);
        assert!(
            values.contains(&gone.into_inner().to_string()) && values.contains("Bool(Some(false))"),
            "{values}"
        );
    }

    #[test]
    fn the_recount_walks_up_from_what_is_not_present() {
        let sql = RECOUNT_MISSING_RUNTIME_DEPS.text();
        assert!(sql.contains("WHERE NOT (EXISTS"), "{sql}");
        assert!(
            sql.contains(
                "SELECT e.derivation FROM derivation_dependency e JOIN incomplete u ON u.derivation = e.dependency WHERE e.kind IN (1, 2)"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("b.missing_runtime_deps <> coalesce(c.n, 0)"),
            "only a row that disagrees is rewritten: {sql}"
        );
    }
}
