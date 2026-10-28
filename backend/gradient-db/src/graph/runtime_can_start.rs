/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The seed cannot read the transition off the row it is writing.
//! The commit making an output present wrote `cached_path` one statement earlier.
//! `freshly_present` is the caller's endpoint for that transition.
//! A freshly present dependency still counts as missing for a referrer in the same seed, since its own ripple subtracts it.
//! A re-push of an already backed path must not count as fresh.

use std::collections::BTreeMap;

use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};

use crate::graph::can_start::{ids, lock_shared_builds};
use crate::graph::predicates::{present_predicate, shared_build_complete_predicate};
use crate::sql::StatusWrite;

fn seed_sql() -> String {
    format!(
        "WITH s(derivation, fresh) AS (SELECT * FROM unnest($1::uuid[], $2::bool[])) \
         UPDATE derivation_build db SET missing_runtime_deps = x.n \
         FROM (SELECT b.id, ({was_complete} AND NOT s.fresh) AS was_complete, \
                      (SELECT count(*) FROM derivation_dependency e \
                         JOIN derivation_build dep ON dep.derivation = e.dependency \
                         LEFT JOIN s landed ON landed.derivation = e.dependency AND landed.fresh \
                        WHERE e.derivation = b.derivation AND e.kind IN (1, 2) \
                          AND NOT ({dep_complete} AND landed.derivation IS NULL))::int AS n \
               FROM s JOIN derivation_build b ON b.derivation = s.derivation) x \
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

fn runtime_dependents_counted(set: &str) -> String {
    format!(
        "counts AS (\
             SELECT s.derivation, count(*)::int AS n FROM {set} u, \
             LATERAL (SELECT e.derivation FROM derivation_dependency e \
                      WHERE e.dependency = u.derivation AND e.kind IN (1, 2) OFFSET 0) s \
             GROUP BY s.derivation)"
    )
}

fn recount_sql() -> String {
    format!(
        "WITH RECURSIVE {incomplete}, {counts} \
         UPDATE derivation_build db SET missing_runtime_deps = coalesce(c.n, 0) \
         FROM derivation_build b LEFT JOIN counts c ON c.derivation = b.derivation \
         WHERE db.id = b.id AND b.missing_runtime_deps <> coalesce(c.n, 0)",
        incomplete = crate::graph::walks::bounded_dependency_closure_cte_body(
            "incomplete",
            &format!(
                "SELECT db.derivation FROM derivation_build db WHERE NOT {}",
                present_predicate("db")
            ),
            crate::graph::walks::ClosureDirection::WantedBy,
            "e.kind IN (1, 2)",
            None,
        ),
        counts = runtime_dependents_counted("incomplete"),
    )
}

fn drifted_sql() -> String {
    format!(
        "WITH marked_incomplete AS MATERIALIZED (\
             SELECT db.derivation, db.missing_runtime_deps FROM derivation_build db \
             WHERE db.missing_runtime_deps <> 0 OR NOT {present}), \
         {counts} \
         SELECT 1 FROM marked_incomplete i FULL JOIN counts c ON c.derivation = i.derivation \
         WHERE i.derivation IS NULL OR i.missing_runtime_deps <> coalesce(c.n, 0) \
         LIMIT 1",
        present = present_predicate("db"),
        counts = runtime_dependents_counted("marked_incomplete"),
    )
}

crate::sql_fn! {
    SEED_MISSING_RUNTIME_DEPS = seed_sql,
        params = [DerivationIds(64), Bools(false, 64)],
        tier = Bulk;

    CLOSURE_COMPLETE_AMONG = complete_among_sql,
        params = [DerivationIds(64)];

    MISSING_RUNTIME_DEPS_DRIFTED = drifted_sql,
        params = [],
        tier = Sweep,
        flags = [Walk];

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
        created = crate::sql::status::to(BuildStatus::RESET_UNCACHED),
        terminal_success = crate::sql::status::from(BuildStatus::RESET_UNCACHED),
    )
}

crate::sql_fn! {
    RESET_UNCACHED_PRODUCERS = reset_uncached_producers_sql,
        params = [DerivationIds(64)],
        status_write = StatusWrite::build(BuildStatus::RESET_UNCACHED);
}

crate::sql_fn! {
    LOCK_CACHED_PATHS = lock_cached_paths_sql,
        params = [CachedPathHashes(64)];
}

crate::sql! {
    DELETE_CACHED_PATHS = "DELETE FROM cached_path cp WHERE cp.hash = ANY($1) RETURNING cp.hash",
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

    transitions.extend(reset_uncached_producers(txn, gone).await?);
    transitions.extend(crate::graph::can_start::unpromote_drv_owners(txn, hashes).await?);

    Ok(transitions)
}

pub(crate) async fn reset_uncached_producers(
    txn: &DatabaseTransaction,
    producers: &[DerivationId],
) -> Result<Vec<crate::status::TransitionChange>, DbErr> {
    if producers.is_empty() {
        return Ok(Vec::new());
    }

    let mut reset = crate::graph::promotion::returned_transitions(
        txn.query_all_raw(RESET_UNCACHED_PRODUCERS.bind([ids(producers)]))
            .await?,
    );
    let thawed: Vec<DerivationId> = reset.iter().map(|c| c.derivation).collect();
    if !thawed.is_empty() {
        let thawed_lock = crate::graph::can_start::lock_seed_shared_builds(txn, &thawed).await?;
        crate::graph::can_start::seed_blocking_deps(&thawed_lock).await?;
        reset.extend(crate::graph::can_start::settle_created(txn, &thawed).await?);
    }

    Ok(reset)
}

pub async fn reset_builds_without_output_paths<C>(
    db: &C,
    producers: &[DerivationId],
) -> Result<Vec<crate::status::TransitionChange>, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    if producers.is_empty() {
        return Ok(Vec::new());
    }

    let txn = db.begin().await?;
    let _lock = lock_shared_builds(&txn, producers).await?;
    let reset = reset_uncached_producers(&txn, producers).await?;
    txn.commit().await?;

    Ok(reset)
}

pub async fn recount_missing_runtime_deps<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    crate::graph::walks::recount_if_drifted(
        db,
        &MISSING_RUNTIME_DEPS_DRIFTED,
        &RECOUNT_MISSING_RUNTIME_DEPS,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{mock_db, raw_statements};
    use sea_orm::Value;

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

    fn ripple_fn() -> &'static str {
        gradient_migration::function_sql("ripple_missing_runtime_deps")
    }

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
        let db = mock_db()
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
        let log = raw_statements(db.into_transaction_log());
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
        let db = mock_db()
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
        let log = raw_statements(db.into_transaction_log());
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
            ripple_fn().contains(&format!(
                "CASE WHEN down THEN {}",
                shared_build_complete_predicate("db")
            )),
            "{}",
            ripple_fn()
        );
        assert!(
            ripple_fn().contains(&format!(
                "ELSE (db.missing_runtime_deps = c.n AND {})",
                crate::graph::predicates::present_value("db")
            )),
            "{}",
            ripple_fn()
        );
        assert_eq!(
            ripple_fn().matches("derivation_dependency").count(),
            ripple_fn().matches("kind IN (1, 2)").count(),
            "every edge the ripple counts is a runtime dependency: {}",
            ripple_fn()
        );
    }

    #[test]
    fn every_level_holds_its_keys_and_ordered_rows_before_it_writes() {
        let keys = ripple_fn()
            .find("PERFORM pg_advisory_xact_lock(643, k)")
            .expect("the keys");
        let rows = ripple_fn()
            .find("WHERE derivation = ANY(parents) ORDER BY derivation FOR NO KEY UPDATE")
            .expect("the ordered row lock");
        let write = ripple_fn()
            .find("SET missing_runtime_deps = db.missing_runtime_deps")
            .expect("the write");
        assert!(keys < rows && rows < write, "{}", ripple_fn());
    }

    #[tokio::test]
    async fn a_retire_ripples_incompleteness_up_from_what_was_complete() {
        let gone = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = mock_db()
            .append_query_results([vec![flag_row(parent, "was_complete", true)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let reached = ripple_shared_builds_incomplete(&txn, vec![gone])
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(reached, vec![parent]);
        let log = raw_statements(db.into_transaction_log());
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
                "LATERAL (SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation AND e.kind IN (1, 2) OFFSET 0) s"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("FROM incomplete u, LATERAL (SELECT e.derivation FROM derivation_dependency e WHERE e.dependency = u.derivation AND e.kind IN (1, 2) OFFSET 0) s"),
            "the counts probe only the edges out of the incomplete set: {sql}"
        );
        assert!(
            sql.contains("b.missing_runtime_deps <> coalesce(c.n, 0)"),
            "only a row that disagrees is rewritten: {sql}"
        );
    }

    #[test]
    fn the_drift_check_counts_from_what_the_counters_mark_incomplete() {
        let sql = MISSING_RUNTIME_DEPS_DRIFTED.text();
        assert!(!sql.contains("RECURSIVE"), "{sql}");
        assert!(
            sql.contains("WHERE db.missing_runtime_deps <> 0 OR NOT (EXISTS"),
            "{sql}"
        );
        assert!(sql.contains("FROM marked_incomplete u, LATERAL ("), "{sql}");
        assert!(
            sql.contains(
                "WHERE i.derivation IS NULL OR i.missing_runtime_deps <> coalesce(c.n, 0)"
            ),
            "a counted dependent the counters mark complete is drift too: {sql}"
        );
    }

    #[tokio::test]
    async fn a_clean_drift_check_skips_the_recount() {
        let db = mock_db()
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([none()])
            .into_connection();

        assert_eq!(recount_missing_runtime_deps(&db).await.unwrap(), 0);
        let log = raw_statements(db.into_transaction_log());
        assert!(
            log.iter()
                .all(|s| !s.sql.contains("SET missing_runtime_deps")),
            "{log:?}"
        );
    }
}
