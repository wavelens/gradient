/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation_build.missing_runtime_deps`: how many of a shared build's runtime dependencies
//! lead to something that is not complete. Complete closure is
//! [`crate::graph::predicates::shared_build_complete_predicate`] - present, with that counter at zero -
//! and it is the second can-start state counter next to `blocking_deps`, one per edge kind.
//! Seeded when a NAR lands, rippled as shared builds become or stop being complete, recounted
//! by the consistency check, which is also the column's backfill.
//!
//! # The seed is absolute, the ripples are relative
//!
//! A relative move must be driven by a TRANSITION or it drives a counter past zero,
//! and a negative counter never satisfies `= 0` again, so the seed reports which rows
//! it flipped and only those ripple. The seed cannot read the transition off the row
//! it writes: presence is a `cached_path` fact and the commit that made an output
//! present wrote it one statement earlier, so a shared build that just became present
//! already reads present when the seed is running while its parents still count it as a
//! missing dependency. `freshly_present` is the caller's endpoint for exactly that, and it is
//! narrow on purpose: a re-push of a path that was already backed changed no
//! presence, and calling it fresh would decrement every parent a second time.
//!
//! The seed moves both ways. A commit can also ADD a runtime dependency into something we
//! do not have, which takes its own shared build out of complete closure, so what the seed
//! un-wholed ripples up in the same pass. The two frontiers are disjoint at the seed
//! and both moves are relative, so a parent reached by both composes.
//!
//! Every pass locks the rows it will write through [`crate::graph::can_start::lock_shared_builds`]
//! or its copy inside the ripple function, which is `derivation`-ordered, and touches
//! `derivation_build` alone: the third class in the lock order, so a caller that
//! already holds `cached_path` may take it.
//! The seed takes [`crate::graph::can_start::lock_seed_shared_builds`] instead, which also holds
//! the dependencies it counts under shared advisory keys, and every frontier a ripple
//! reads the edges of is held under its exclusive keys by then; that pairing, not a
//! single writer, is what keeps a seed and a concurrent flip from each missing the
//! other's rows ([`crate::graph::shared_build_guard`]).

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

/// The sweep's absolute recount and the column's backfill: `incomplete` is everything
/// not present and, transitively, everything with a runtime dependency into it, so an
/// shared build's count is its runtime dependencies inside that set.
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
    /// Recount the shared builds the caller names against their runtime dependencies as they
    /// stand. `fresh` names a shared build whose outputs this transaction made present,
    /// which was not complete before it whatever the row now says.
    ///
    /// `Bulk` for the reason the can-start state seed it mirrors is: a batch that reads
    /// the edges of every value it was handed costs more than one row lookup per
    /// value, which is all the hot tier's ceiling allows for.
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
    /// The hashes among `$1` whose path is in our cache and whose producing shared build
    /// is complete. An output with no shared build row yet counts on presence alone, which
    /// is what that shared build's first seed concludes anyway.
    COMPLETE_OUTPUT_HASHES = complete_output_hashes_sql,
        params = [CachedPathHashes(64)],
        tier = Bulk;
}

/// The subset of `hashes` an evaluation already has complete in our own cache.
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

/// The hash-ordered `FOR NO KEY UPDATE` pass every retire opens with. It conflicts with
/// the RI `FOR KEY SHARE` a concurrent `cached_path_signature` insert holds on the
/// parent row, so that wait is absorbed in its own statement and the DELETE opens
/// a snapshot that sees the signature it would otherwise cascade away. It also holds
/// the producers' advisory keys exclusively, ahead of the rows, so the statements
/// after it read their complete closure from a snapshot that includes any flip they waited
/// for ([`crate::graph::shared_build_guard`]). It reads nothing and decides nothing.
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

    /// The whole ripple in one call, `RIPPLE_MISSING_RUNTIME_DEPS_FN` in the
    /// migration that defines it: every level's runtime parents counted, held
    /// under their keys and rows in `derivation` order and moved by their edge
    /// count, in place rather than a round trip per level. Returns every shared build
    /// that flipped.
    RIPPLE_MISSING_RUNTIME_DEPS = "SELECT derivation FROM ripple_missing_runtime_deps($1::uuid[], $2::bool) AS r(derivation)",
        params = [DerivationIds(64), Bool(true)],
        tier = Bulk;
}

/// What a seed moved: the shared builds that became complete, and the ones that stopped
/// being complete, each with everything the ripple reached from them.
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

/// Seed the counter of every shared build an event touched and ripple what flipped.
/// `freshly_present` are the shared builds whose outputs this transaction made present,
/// `recounted` the ones whose runtime dependencies it merely grew.
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

/// Count down the runtime parents of shared builds that became complete, level by level,
/// and report everything that became complete with them.
pub async fn ripple_shared_builds_complete(
    txn: &DatabaseTransaction,
    frontier: Vec<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    ripple(txn, frontier, true).await
}

/// The mirror: count up the parents of shared builds that stopped being complete.
pub async fn ripple_shared_builds_incomplete(
    txn: &DatabaseTransaction,
    frontier: Vec<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    ripple(txn, frontier, false).await
}

/// Move the counters above `frontier` level by level in one call, `down` from
/// shared builds that became complete, up from shared builds that were, and return every shared build
/// that flipped. Each level is held under its keys and rows in `derivation` order
/// before it is written, the module doc's discipline, inside the function.
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

/// Take `hashes` `FOR NO KEY UPDATE` in one hash-ordered statement, before the caller
/// decides or writes anything. A transaction that will write `derivation_build`
/// and only then reach a retire has to take its `cached_path` locks FIRST, or it
/// inverts the class order every other writer follows; re-acquiring a row this
/// transaction already holds is free, so the retire's own pass repeats it for
/// nothing.
pub async fn lock_cached_paths(txn: &DatabaseTransaction, hashes: &[String]) -> Result<(), DbErr> {
    if hashes.is_empty() {
        return Ok(());
    }

    txn.execute_raw(LOCK_CACHED_PATHS.bind([hashes.to_vec().into()]))
        .await?;

    Ok(())
}

/// The shared builds among `derivations` that are complete right now, held `FOR NO KEY UPDATE`.
/// Read BEFORE the event that takes their presence away, because nothing after it
/// can recover the endpoint, and only after their advisory keys are held by an
/// earlier statement: [`retire_outputs`] opens with [`LOCK_CACHED_PATHS`], which
/// takes them.
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

/// What a retire removed and what it moved: the shared builds that stopped being complete,
/// and the transitions the caller emits once its transaction has committed.
#[derive(Debug, Default)]
pub struct Retired {
    pub deleted: Vec<String>,
    pub incomplete: Vec<DerivationId>,
    pub transitions: Vec<crate::status::TransitionChange>,
}

/// Delete `hashes` from `cached_path` and take every shared build that trusted them out
/// of complete closure: the producers of the deleted paths lose presence, their
/// parents over runtime dependencies count up, and the can-start state side follows.
///
/// The shared builds that WERE complete are read before the delete, under the shared build lock,
/// because nothing after it can recover that endpoint. The `cached_path` rows are
/// locked first, in hash order, so the wait for a concurrent
/// `cached_path_signature` insert is absorbed in its own statement and the DELETE
/// opens a snapshot that sees it; taking the shared build lock second is the class order
/// the whole module obeys.
///
/// The mark follows the union of what is gone and what the ripple un-wholed, but
/// the RESET is narrower: only the producers of what is actually gone. A parent
/// that merely lost complete closure still has its own output, so it needs `fetchable` to
/// drop and nothing else, and the arrival of the missing path marks it fetchable
/// again through a terminal status a reset would have taken away. Resetting the
/// closure instead re-queued 107 derivations from deleting one NAR. The mark itself
/// re-opens the walk below what it flipped, which is how the missing path is asked
/// for: dropping the flag and asking for nothing left 47 builders waiting behind 22
/// incomplete `Completed` shared builds nobody named.
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

/// The can-start state half of a retire. `gone` are the producers whose artifact is
/// absent, `incomplete` everything the ripple took out of complete closure with them.
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

/// The sweep's recount, and the backfill after the column's migration.
pub async fn recount_missing_runtime_deps<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db).await?;
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

    /// The keys have to be held before the statement that reads complete closure starts:
    /// a statement that waits for a key inside itself still reads the snapshot it
    /// took before the wait, and misses the flip it waited for.
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

    /// A row that was complete before the seed and after it flipped nothing, so no
    /// parent ever counted it as a missing dependency and it must not count them down; only a
    /// row the seed flipped ripples, and the ripple stops at a level that flips
    /// nothing.
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

    /// The commit that makes an output present writes `cached_path` one statement
    /// before the seed, so a freshly present shared build reads complete on both sides of it.
    /// Its parents counted it as a missing dependency and are waiting for the count-down, so
    /// the caller's "this transaction made it present" is the endpoint, not the row.
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

    /// The function decides a flip the way the module does: down, a shared build
    /// continues the ripple when it reads complete; up, when its counter was at zero
    /// and its outputs are present, read in one probe and only behind the counter.
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

    /// Every write of the counter takes the level's keys, then its rows in
    /// `derivation` order, so two ripples over overlapping frontiers cannot cycle.
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

    /// A retire is the mirror of a commit: the shared builds that WERE complete are read
    /// before the artifact goes, and incompleteness ripples up from exactly those.
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

    /// The recount walks UP from what is not present, so a shared build's count is its
    /// runtime dependencies into that set and the whole chain converges in one pass.
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
