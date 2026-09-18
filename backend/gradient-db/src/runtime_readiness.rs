/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation_build.missing_runtime_deps`: how many of an anchor's runtime edges
//! lead to something that is not whole. Wholeness is
//! [`crate::graph_sql::anchor_whole_predicate`] - present, with that counter at zero -
//! and it is the second readiness counter next to `unready_deps`, one per edge kind.
//! Seeded when a NAR lands, rippled as anchors become or stop being whole, recounted
//! by the consistency sweep, which is also the column's backfill.
//!
//! # The seed is absolute, the ripples are relative
//!
//! A relative move must be driven by a TRANSITION or it drives a counter past zero,
//! and a negative counter never satisfies `= 0` again, so the seed reports which rows
//! it flipped and only those ripple. The seed cannot read the transition off the row
//! it writes: presence is a `cached_path` fact and the commit that made an output
//! present wrote it one statement earlier, so an anchor that just became present
//! already reads present when the seed runs while its dependents still count it as a
//! hole. `freshly_present` is the caller's endpoint for exactly that, and it is
//! narrow on purpose: a re-push of a path that was already backed changed no
//! presence, and calling it fresh would decrement every dependent a second time.
//!
//! The seed moves both ways. A commit can also ADD a runtime edge into something we
//! do not have, which takes its own anchor out of wholeness, so what the seed
//! un-wholed ripples up in the same pass. The two frontiers are disjoint at the seed
//! and both moves are relative, so a dependent reached by both composes.
//!
//! Every pass locks the rows it will write through [`crate::readiness::lock_anchors`],
//! which is `derivation`-ordered, and touches `derivation_build` alone: the third
//! class in the lock order, so a caller that already holds `cached_path` may take it.

use std::collections::BTreeMap;

use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};

use crate::graph_sql::{anchor_whole_predicate, present_predicate};
use crate::readiness::{ids, lock_anchors};

fn seed_sql() -> String {
    format!(
        "UPDATE derivation_build db SET missing_runtime_deps = x.n \
         FROM (SELECT b.id, ({was_whole} AND NOT s.fresh) AS was_whole, \
                      (SELECT count(*) FROM derivation_dependency e \
                         JOIN derivation_build dep ON dep.derivation = e.dependency \
                        WHERE e.derivation = b.derivation AND e.kind IN (1, 2) \
                          AND NOT {dep_whole})::int AS n \
               FROM unnest($1::uuid[], $2::bool[]) AS s(derivation, fresh) \
               JOIN derivation_build b ON b.derivation = s.derivation) x \
         WHERE db.id = x.id \
         RETURNING db.derivation, x.was_whole, {whole} AS whole",
        was_whole = anchor_whole_predicate("b"),
        dep_whole = anchor_whole_predicate("dep"),
        whole = anchor_whole_predicate("db"),
    )
}

fn count_down_sql() -> String {
    format!(
        "UPDATE derivation_build db SET missing_runtime_deps = db.missing_runtime_deps - c.n \
         FROM unnest($1::uuid[], $2::int[]) AS c(derivation, n) \
         WHERE db.derivation = c.derivation \
         RETURNING db.derivation, {whole} AS whole",
        whole = anchor_whole_predicate("db"),
    )
}

fn count_up_sql() -> String {
    format!(
        "UPDATE derivation_build db SET missing_runtime_deps = db.missing_runtime_deps + c.n \
         FROM unnest($1::uuid[], $2::int[]) AS c(derivation, n) \
         WHERE db.derivation = c.derivation \
         RETURNING db.derivation, ({present} AND db.missing_runtime_deps = c.n) AS was_whole",
        present = present_predicate("db"),
    )
}

fn whole_among_sql() -> String {
    format!(
        "SELECT db.derivation FROM derivation_build db \
         WHERE db.derivation = ANY($1::uuid[]) AND {whole} \
         ORDER BY db.derivation FOR UPDATE",
        whole = anchor_whole_predicate("db"),
    )
}

/// The sweep's absolute recount and the column's backfill: `unwhole` is everything
/// not present and, transitively, everything with a runtime edge into it, so an
/// anchor's count is its runtime edges inside that set.
fn recount_sql() -> String {
    format!(
        "WITH RECURSIVE unwhole(derivation) AS (\
             SELECT db.derivation FROM derivation_build db WHERE NOT {present} \
             UNION \
             SELECT e.derivation FROM derivation_dependency e \
             JOIN unwhole u ON u.derivation = e.dependency WHERE e.kind IN (1, 2)), \
         counts AS (\
             SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e \
             JOIN unwhole u ON u.derivation = e.dependency WHERE e.kind IN (1, 2) \
             GROUP BY e.derivation) \
         UPDATE derivation_build db SET missing_runtime_deps = coalesce(c.n, 0) \
         FROM derivation_build b LEFT JOIN counts c ON c.derivation = b.derivation \
         WHERE db.id = b.id AND b.missing_runtime_deps <> coalesce(c.n, 0)",
        present = present_predicate("db"),
    )
}

crate::sql_fn! {
    /// Recount the anchors the caller names against their runtime edges as they
    /// stand. `fresh` names an anchor whose outputs this transaction made present,
    /// which was not whole before it whatever the row now says.
    ///
    /// `Bulk` for the reason the readiness seed it mirrors is: a batch that reads
    /// the edges of every value it was handed costs more than one row lookup per
    /// value, which is all the hot tier's ceiling allows for.
    SEED_MISSING_RUNTIME_DEPS = seed_sql,
        params = [DerivationIds(64), Bools(false, 64)],
        tier = Bulk;

    COUNT_DOWN_RUNTIME = count_down_sql,
        params = [DerivationIds(64), Ints(1, 64)];

    COUNT_UP_RUNTIME = count_up_sql,
        params = [DerivationIds(64), Ints(1, 64)];

    WHOLE_AMONG = whole_among_sql,
        params = [DerivationIds(64)];

    RECOUNT_MISSING_RUNTIME_DEPS = recount_sql,
        params = [],
        tier = Sweep,
        flags = [Walk];
}

crate::sql! {
    RUNTIME_DEPENDENT_COUNTS = "SELECT e.derivation AS derivation, count(*)::int AS n \
                                FROM derivation_dependency e \
                                WHERE e.dependency = ANY($1::uuid[]) AND e.kind IN (1, 2) \
                                GROUP BY e.derivation ORDER BY e.derivation",
        params = [DerivationIds(64)];
}

/// What a seed moved: the anchors that became whole, and the ones that stopped
/// being whole, each with everything the ripple reached from them.
#[derive(Debug, Default)]
pub struct Seeded {
    pub whole: Vec<DerivationId>,
    pub unwhole: Vec<DerivationId>,
}

fn derivation_ids(rows: &[QueryResult]) -> Vec<DerivationId> {
    rows.iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

fn flagged(rows: &[QueryResult], flag: &str) -> Vec<DerivationId> {
    rows.iter()
        .filter(|r| r.try_get::<bool>("", flag).unwrap_or(false))
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

/// Seed the counter of every anchor an event touched and ripple what flipped.
/// `freshly_present` are the anchors whose outputs this transaction made present,
/// `recounted` the ones whose runtime edges it merely grew.
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
    let _lock = lock_anchors(txn, &sorted).await?;
    let rows = txn
        .query_all_raw(SEED_MISSING_RUNTIME_DEPS.bind([ids(&sorted), fresh.into()]))
        .await?;

    let mut moved = Seeded::default();
    for row in &rows {
        let (Ok(derivation), Ok(was_whole), Ok(whole)) = (
            row.try_get::<uuid::Uuid>("", "derivation"),
            row.try_get::<bool>("", "was_whole"),
            row.try_get::<bool>("", "whole"),
        ) else {
            continue;
        };

        match (was_whole, whole) {
            (false, true) => moved.whole.push(DerivationId::new(derivation)),
            (true, false) => moved.unwhole.push(DerivationId::new(derivation)),
            _ => {}
        }
    }

    let gained = moved.whole.clone();
    let lost = moved.unwhole.clone();
    moved.whole.extend(ripple_anchors_whole(txn, gained).await?);
    moved
        .unwhole
        .extend(ripple_anchors_unwhole(txn, lost).await?);

    Ok(moved)
}

/// Count down the runtime dependents of anchors that became whole, level by level,
/// and report everything that became whole with them.
pub async fn ripple_anchors_whole(
    txn: &DatabaseTransaction,
    frontier: Vec<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    ripple(txn, &COUNT_DOWN_RUNTIME, "whole", frontier).await
}

/// The mirror: count up the dependents of anchors that stopped being whole.
pub async fn ripple_anchors_unwhole(
    txn: &DatabaseTransaction,
    frontier: Vec<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    ripple(txn, &COUNT_UP_RUNTIME, "was_whole", frontier).await
}

/// One level per statement: the runtime dependents of `frontier` are counted, locked
/// in `derivation` order and moved by their edge count; the ones `flag` names form
/// the next level.
///
/// The counts are read separately so the update is a nested loop over a bound array
/// and takes its row locks in the module doc's order rather than in plan order,
/// which is what a derived set deadlocked on.
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
            .query_all_raw(RUNTIME_DEPENDENT_COUNTS.bind([ids(&frontier)]))
            .await?
        {
            dependents.push(DerivationId::new(
                row.try_get::<uuid::Uuid>("", "derivation")?,
            ));
            counts.push(row.try_get::<i32>("", "n")?);
        }
        if dependents.is_empty() {
            break;
        }

        let _lock = lock_anchors(txn, &dependents).await?;
        let rows = txn
            .query_all_raw(step.bind([ids(&dependents), counts.into()]))
            .await?;
        frontier = flagged(&rows, flag);
        reached.extend(frontier.iter().copied());
    }

    Ok(reached)
}

/// The anchors among `derivations` that are whole right now, held `FOR UPDATE`.
/// Read BEFORE the event that takes their presence away, because nothing after it
/// can recover the endpoint.
pub async fn whole_among(
    txn: &DatabaseTransaction,
    derivations: &[DerivationId],
) -> Result<Vec<DerivationId>, DbErr> {
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    Ok(derivation_ids(
        &txn.query_all_raw(WHOLE_AMONG.bind([ids(derivations)]))
            .await?,
    ))
}

/// What a retire removed and what it moved: the anchors that stopped being whole,
/// and the transitions the caller emits once its transaction has committed.
#[derive(Debug, Default)]
pub struct Retired {
    pub deleted: Vec<String>,
    pub unwhole: Vec<DerivationId>,
    pub transitions: Vec<crate::status::TransitionChange>,
}

/// Delete `hashes` from `cached_path` and take every anchor that trusted them out
/// of wholeness: the producers of the deleted paths lose presence, their dependents
/// over runtime edges count up, and the readiness side follows.
///
/// The anchors that WERE whole are read before the delete, under the anchor lock,
/// because nothing after it can recover that endpoint. The `cached_path` rows are
/// locked first, in hash order, so the wait for a concurrent
/// `cached_path_signature` insert is absorbed in its own statement and the DELETE
/// opens a snapshot that sees it; taking the anchor lock second is the class order
/// the whole module obeys.
///
/// The mark follows the union of what is gone and what the ripple un-wholed, but
/// the RESET is narrower: only the producers of what is actually gone. A dependent
/// that merely lost wholeness still has its own output, so it needs `fetchable` to
/// drop and nothing else, and the arrival of the missing path marks it fetchable
/// again through a terminal status a reset would have taken away. Resetting the
/// closure instead re-queued 107 derivations from deleting one NAR.
pub async fn retire_outputs(
    txn: &DatabaseTransaction,
    hashes: &[String],
) -> Result<Retired, DbErr> {
    if hashes.is_empty() {
        return Ok(Retired::default());
    }

    txn.execute_raw(crate::nar_closure::LOCK_QUERY.bind([hashes.to_vec().into()]))
        .await?;

    let gone = crate::reachability::producers_of_hashes(txn, hashes).await?;
    let was_whole = whole_among(txn, &gone).await?;

    let deleted: Vec<String> = txn
        .query_all_raw(crate::nar_closure::DELETE_STATEMENT.bind([hashes.to_vec().into()]))
        .await?
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect();

    if !deleted.is_empty() {
        txn.execute_raw(crate::nar_closure::SET_OUTPUTS_UNCACHED.bind([deleted.clone().into()]))
            .await?;
    }

    let mut unwhole = was_whole.clone();
    unwhole.extend(ripple_anchors_unwhole(txn, was_whole).await?);
    let transitions = retire_anchors(txn, &gone, &unwhole, hashes).await?;

    Ok(Retired {
        deleted,
        unwhole,
        transitions,
    })
}

/// The readiness half of a retire. `gone` are the producers whose artifact is
/// absent, `unwhole` everything the ripple took out of wholeness with them.
async fn retire_anchors(
    txn: &DatabaseTransaction,
    gone: &[DerivationId],
    unwhole: &[DerivationId],
    hashes: &[String],
) -> Result<Vec<crate::status::TransitionChange>, DbErr> {
    let mut scope = gone.to_vec();
    scope.extend_from_slice(unwhole);
    scope.sort_unstable();
    scope.dedup();

    let lock = lock_anchors(txn, &scope).await?;
    let mut transitions = crate::readiness::lost_fetchability(&lock).await?;

    if !gone.is_empty() {
        let reset = crate::promotion::returned_transitions(
            txn.query_all_raw(crate::nar_closure::RESET_UNCACHED_PRODUCERS.bind([ids(gone)]))
                .await?,
        );
        if !reset.is_empty() {
            let thawed: Vec<DerivationId> = reset.iter().map(|c| c.derivation).collect();
            let thawed_lock = lock_anchors(txn, &thawed).await?;
            crate::readiness::seed_unready_deps(&thawed_lock).await?;
        }

        transitions.extend(reset);
    }

    transitions.extend(crate::readiness::unpromote_drv_owners(txn, hashes).await?);

    Ok(transitions)
}

/// The sweep's recount, and the backfill after the column's migration.
pub async fn recount_missing_runtime_deps<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
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

    fn seed_row(id: DerivationId, was_whole: bool, whole: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("was_whole".to_owned(), Value::from(was_whole)),
            ("whole".to_owned(), Value::from(whole)),
        ])
    }

    fn flag_row(id: DerivationId, flag: &str, value: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            (flag.to_owned(), Value::from(value)),
        ])
    }

    fn count_row(id: DerivationId, n: i32) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("n".to_owned(), Value::Int(Some(n))),
        ])
    }

    fn none() -> Vec<BTreeMap<String, Value>> {
        Vec::new()
    }

    /// A row that was whole before the seed and after it flipped nothing, so no
    /// dependent ever counted it as a hole and it must not count them down; only a
    /// row the seed flipped ripples, and the ripple stops at a level that flips
    /// nothing.
    #[tokio::test]
    async fn only_an_anchor_the_seed_flipped_ripples() {
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
            .append_query_results([vec![count_row(parent, 1)]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![flag_row(parent, "whole", true)]])
            .append_query_results([none()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let moved = seed_runtime_deps(&txn, &[], &[settled, flipped])
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(moved.whole, vec![flipped, parent]);
        assert!(moved.unwhole.is_empty());
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let counts: Vec<String> = log
            .iter()
            .filter(|s| {
                s.sql
                    .contains("count(*)::int AS n FROM derivation_dependency e")
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

    /// The commit that makes an output present writes `cached_path` one statement
    /// before the seed, so a freshly present anchor reads whole on both sides of it.
    /// Its dependents counted it as a hole and are waiting for the count-down, so
    /// the caller's "this transaction made it present" is the endpoint, not the row.
    #[tokio::test]
    async fn a_freshly_present_anchor_ripples_though_the_row_reads_whole_throughout() {
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

        assert_eq!(moved.whole, vec![landed]);
        let log = crate::pool::raw_statements(db.into_transaction_log());
        let seed = log
            .iter()
            .find(|s| s.sql.contains("SET missing_runtime_deps = x.n"))
            .expect("the seed runs");
        assert!(
            seed.sql.contains("AND NOT s.fresh) AS was_whole"),
            "the caller's endpoint has to beat the row: {}",
            seed.sql
        );
    }

    /// Every write of the counter takes the ordered anchor lock first, so two
    /// ripples over overlapping frontiers cannot cycle.
    #[tokio::test]
    async fn every_update_follows_an_ordered_lock_on_anchor_rows() {
        let seed = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![seed_row(seed, false, true)]])
            .append_query_results([vec![count_row(parent, 1)]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![flag_row(parent, "whole", false)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        seed_runtime_deps(&txn, &[], &[seed]).await.unwrap();
        txn.commit().await.unwrap();

        let log = crate::pool::raw_statements(db.into_transaction_log());
        for (i, stmt) in log.iter().enumerate() {
            if stmt.sql.contains("SET missing_runtime_deps") {
                assert!(
                    log[..i]
                        .iter()
                        .any(|s| s.sql.contains("ORDER BY derivation FOR UPDATE")),
                    "an unlocked counter write: {log:?}"
                );
            }
        }
    }

    /// A retire is the mirror of a commit: the anchors that WERE whole are read
    /// before the artifact goes, and unwholeness ripples up from exactly those.
    #[tokio::test]
    async fn a_retire_ripples_unwholeness_up_from_what_was_whole() {
        let gone = DerivationId::now_v7();
        let parent = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(parent, 2)]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![flag_row(parent, "was_whole", true)]])
            .append_query_results([none()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let reached = ripple_anchors_unwhole(&txn, vec![gone]).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(reached, vec![parent]);
        let log = crate::pool::raw_statements(db.into_transaction_log());
        assert!(
            log.iter()
                .any(|s| s.sql.contains("missing_runtime_deps + c.n")),
            "{log:?}"
        );
    }

    /// The recount walks UP from what is not present, so an anchor's count is its
    /// runtime edges into that set and the whole chain converges in one pass.
    #[test]
    fn the_recount_walks_up_from_what_is_not_present() {
        let sql = RECOUNT_MISSING_RUNTIME_DEPS.text();
        assert!(sql.contains("WHERE NOT (EXISTS"), "{sql}");
        assert!(
            sql.contains(
                "SELECT e.derivation FROM derivation_dependency e JOIN unwhole u ON u.derivation = e.dependency WHERE e.kind IN (1, 2)"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("b.missing_runtime_deps <> coalesce(c.n, 0)"),
            "only a row that disagrees is rewritten: {sql}"
        );
    }
}
