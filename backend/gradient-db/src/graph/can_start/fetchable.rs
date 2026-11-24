/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::predicates::{builds_with_unneeded_output_predicate, fetchable_predicate};
use crate::graph::promotion::{returned_derivations, returned_transitions};
use crate::sql::StatusWrite;
use crate::status::TransitionChange;

use super::lock::{SeedLock, SharedBuildLock, ids, lock_shared_builds};
use super::need::{settle_need, update_need};
use super::queue::promote;
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use std::sync::LazyLock;

/// Left joins count a dependency without a shared build row as blocking.
/// Inner joins fail open and start a dispatch against a missing input.
/// Runtime-only edges are no build inputs and belong to `missing_runtime_deps`.
pub(super) fn blocking_dependency_count(alias: &str) -> String {
    format!(
        "(SELECT count(*) FROM derivation_dependency e \
         LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
         WHERE e.derivation = {alias}.derivation AND e.kind IN (0, 2) \
           AND (dep.derivation IS NULL OR NOT dep.fetchable OR {unneeded}))",
        unneeded = builds_with_unneeded_output_predicate("e", "dep"),
    )
}

fn parents_counted(alias: &str) -> String {
    format!(
        "(SELECT e.derivation, count(*) AS n FROM derivation_dependency e \
         JOIN derivation_build dep ON dep.derivation = e.dependency \
         WHERE e.dependency = ANY($1::uuid[]) AND e.kind IN (0, 2) AND NOT {unneeded} \
         GROUP BY e.derivation) {alias}",
        unneeded = builds_with_unneeded_output_predicate("e", "dep"),
    )
}

pub(super) static SEED_BLOCKING_DEPS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET blocking_deps = {count} \
         WHERE db.derivation = ANY($1::uuid[])",
        count = blocking_dependency_count("db"),
    )
});

crate::sql_lazy! {
    SEED_BLOCKING_DEPS_QUERY = || SEED_BLOCKING_DEPS.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

static MARK_FETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = true \
         WHERE db.derivation = ANY($1::uuid[]) AND NOT db.fetchable AND {pred} \
         RETURNING db.derivation",
        pred = fetchable_predicate("db"),
    )
});

crate::sql_lazy! {
    MARK_FETCHABLE_QUERY = || MARK_FETCHABLE.as_str(),
        params = [DerivationIds(64)];
}

static MARK_UNFETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = false \
         WHERE db.derivation = ANY($1::uuid[]) AND db.fetchable AND NOT {pred} \
         RETURNING db.derivation",
        pred = fetchable_predicate("db"),
    )
});

crate::sql_lazy! {
    MARK_UNFETCHABLE_QUERY = || MARK_UNFETCHABLE.as_str(),
        params = [DerivationIds(64)];
}

fn unblock_parents_sql() -> String {
    format!(
        "UPDATE derivation_build d SET blocking_deps = d.blocking_deps - c.n \
         FROM {counted} WHERE d.derivation = c.derivation \
         RETURNING d.derivation, d.blocking_deps = 0 AS startable",
        counted = parents_counted("c"),
    )
}

crate::sql_fn! {
    UNBLOCK_PARENTS = unblock_parents_sql,
        params = [DerivationIds(64)],
        tier = Bulk;
}

fn parent_build_blocking_dep_count_recount_subquery(alias: &str) -> String {
    format!(
        "(SELECT p.derivation, {count} AS n FROM derivation_build p \
         WHERE p.derivation IN (SELECT e.derivation FROM derivation_dependency e \
                                WHERE e.dependency = ANY($1::uuid[]) AND e.kind IN (0, 2))) {alias}",
        count = blocking_dependency_count("p"),
    )
}

crate::sql! {
    LOCK_PARENT_BUILDS_OF_UNFETCHABLE_DEPENDENCIES_IN_DERIVATION_ID_ORDER =
        "SELECT 1 FROM derivation_build p \
         WHERE p.derivation IN (SELECT e.derivation FROM derivation_dependency e \
                                WHERE e.dependency = ANY($1::uuid[]) AND e.kind IN (0, 2)) \
         ORDER BY p.derivation FOR NO KEY UPDATE",
        params = [DerivationIds(64)],
        tier = Bulk;
}

// Fetchability losses remove the stored output row for the trust check. The code can lose the
// earlier edge state, so parents receive only a recount, not an incremental adjustment. All locks
// acquire the parent row first. Second fetchability changes for another dependency of the same
// parent need to wait for this commit and then recount with the first change visible.
static BLOCK_PARENTS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build d \
         SET blocking_deps = c.n, \
             status = CASE WHEN {unqueue} THEN {created} ELSE d.status END, \
             updated_at = CASE WHEN {unqueue} \
                               THEN (now() AT TIME ZONE 'UTC') ELSE d.updated_at END \
         FROM {counted} \
         WHERE d.derivation = c.derivation AND d.blocking_deps <> c.n \
         RETURNING d.derivation, old.status AS from_status, d.status AS to_status",
        counted = parent_build_blocking_dep_count_recount_subquery("c"),
        unqueue = format!(
            "c.n > 0 AND d.status = {queued} AND {not_in_flight}",
            queued = crate::sql::status::from(BuildStatus::UNQUEUE),
            not_in_flight = crate::scheduling::assignment_record::no_open_assignment_predicate(
                &crate::scheduling::assignment_record::build_job_key_sql("d.id")
            ),
        ),
        created = crate::sql::status::to(BuildStatus::UNQUEUE),
    )
});

crate::sql_lazy! {
    BLOCK_PARENTS_QUERY = || BLOCK_PARENTS.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk,
        status_write = StatusWrite::build(BuildStatus::UNQUEUE);
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn seed_blocking_deps(lock: &SeedLock<'_>) -> Result<u64, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(0);
    }

    Ok(lock
        .txn
        .execute_raw(SEED_BLOCKING_DEPS_QUERY.bind([ids(&lock.derivations)]))
        .await?
        .rows_affected())
}

async fn mark(lock: &SharedBuildLock<'_>, to: bool) -> Result<Vec<DerivationId>, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(Vec::new());
    }

    let query = if to {
        &MARK_FETCHABLE_QUERY
    } else {
        &MARK_UNFETCHABLE_QUERY
    };

    Ok(returned_derivations(
        lock.txn
            .query_all_raw(query.bind([ids(&lock.derivations)]))
            .await?,
    ))
}

#[derive(Debug, Default)]
pub(super) struct MarkedFetchable {
    pub(super) marked: Vec<DerivationId>,
    pub(super) startable: Vec<DerivationId>,
}

pub(super) async fn mark_fetchable(lock: &SharedBuildLock<'_>) -> Result<MarkedFetchable, DbErr> {
    let marked = mark(lock, true).await?;
    if marked.is_empty() {
        return Ok(MarkedFetchable::default());
    }

    let rows = lock
        .txn
        .query_all_raw(UNBLOCK_PARENTS.bind([ids(&marked)]))
        .await?;

    let mut startable = Vec::new();
    for row in rows {
        if row.try_get::<bool>("", "startable")? {
            startable.push(DerivationId::new(
                row.try_get::<uuid::Uuid>("", "derivation")?,
            ));
        }
    }

    Ok(MarkedFetchable { marked, startable })
}

#[derive(Debug, Default)]
pub(super) struct MarkedUnfetchable {
    pub(super) marked: Vec<DerivationId>,
    pub(super) unqueued: Vec<TransitionChange>,
}

pub(super) async fn mark_unfetchable(
    lock: &SharedBuildLock<'_>,
) -> Result<MarkedUnfetchable, DbErr> {
    let marked = mark(lock, false).await?;
    if marked.is_empty() {
        return Ok(MarkedUnfetchable::default());
    }

    lock.txn
        .execute_raw(
            LOCK_PARENT_BUILDS_OF_UNFETCHABLE_DEPENDENCIES_IN_DERIVATION_ID_ORDER
                .bind([ids(&marked)]),
        )
        .await?;
    let rows = lock
        .txn
        .query_all_raw(BLOCK_PARENTS_QUERY.bind([ids(&marked)]))
        .await?;
    let unqueued = returned_transitions(rows)
        .into_iter()
        .filter(|c| c.from != c.to)
        .collect();

    Ok(MarkedUnfetchable { marked, unqueued })
}

#[derive(Debug, Default)]
pub struct FetchableStatusChangesAndNewlyMarkedDerivations {
    pub changes: Vec<TransitionChange>,
    pub marked: Vec<DerivationId>,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn became_fetchable(
    lock: &SharedBuildLock<'_>,
) -> Result<FetchableStatusChangesAndNewlyMarkedDerivations, DbErr> {
    let marked = mark_fetchable(lock).await?;
    let mut changes = promote(lock.txn, &marked.startable).await?;
    if !marked.marked.is_empty() {
        let moved = update_need(lock.txn, &marked.marked).await?;
        changes.extend(settle_need(lock.txn, &moved).await?);
    }

    Ok(FetchableStatusChangesAndNewlyMarkedDerivations {
        changes,
        marked: marked.marked,
    })
}

pub async fn advance_fetchable<C>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<FetchableStatusChangesAndNewlyMarkedDerivations, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if derivations.is_empty() {
        return Ok(FetchableStatusChangesAndNewlyMarkedDerivations::default());
    }

    let txn = db.begin().await?;
    let lock = lock_shared_builds(&txn, derivations).await?;
    let fetchable = became_fetchable(&lock).await?;
    txn.commit().await?;

    Ok(fetchable)
}

pub async fn lost_fetchability(lock: &SharedBuildLock<'_>) -> Result<Vec<TransitionChange>, DbErr> {
    let MarkedUnfetchable {
        marked,
        mut unqueued,
    } = mark_unfetchable(lock).await?;
    if marked.is_empty() {
        return Ok(unqueued);
    }

    let moved = update_need(lock.txn, &marked).await?;
    unqueued.extend(settle_need(lock.txn, &moved).await?);

    Ok(unqueued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::lock::lock_seed_shared_builds;
    use crate::graph::can_start::queue::unpromote_ungated;
    use crate::graph::can_start::repair::RECOUNT_BLOCKING_DEPS;
    use crate::graph::can_start::test_rows::{drv, exec, norm, transition_row};
    use gradient_fixtures::mock::{empty_db, mock_db, statements};
    use sea_orm::Value;
    use std::collections::BTreeMap;

    fn ripple_row(id: DerivationId, startable: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("startable".to_owned(), Value::from(startable)),
        ])
    }

    #[test]
    fn the_build_gate_counts_build_edges_only() {
        for sql in [
            SEED_BLOCKING_DEPS.to_string(),
            RECOUNT_BLOCKING_DEPS.to_string(),
            UNBLOCK_PARENTS.text().into_owned(),
            BLOCK_PARENTS.to_string(),
        ] {
            assert!(sql.contains("e.kind IN (0, 2)"), "{sql}");
        }
    }

    #[test]
    fn the_count_treats_a_dependency_with_no_shared_build_as_blocking() {
        let sql = norm(&blocking_dependency_count("db"));
        assert_eq!(
            sql,
            norm(
                "(SELECT count(*) FROM derivation_dependency e \
                 LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
                 WHERE e.derivation = db.derivation \
                 AND e.kind IN (0, 2) \
                 AND (dep.derivation IS NULL OR NOT dep.fetchable \
                 OR (dep.unneeded_outputs <> '{}' AND (e.dependency_outputs IS NULL \
                 OR e.dependency_outputs && dep.unneeded_outputs))))"
            )
        );
    }

    #[test]
    fn a_ripple_moves_only_the_edges_the_count_reads_through_the_flag() {
        let unneeded = norm(&builds_with_unneeded_output_predicate("e", "dep"));
        let sql = UNBLOCK_PARENTS.text().into_owned();
        assert!(
            norm(&sql).contains(&format!("AND NOT {unneeded} GROUP BY e.derivation")),
            "an edge blocked by an unneeded output never moved with the flag: {sql}"
        );
    }

    #[test]
    fn blocking_parent_builds_recounts_blocking_dependency_count() {
        let sql = norm(&BLOCK_PARENTS);
        assert!(
            sql.contains(&norm(&blocking_dependency_count("p"))),
            "{sql}"
        );
    }

    #[test]
    fn the_seed_counts_the_column_every_mark_adjusts_parents_by() {
        let seed = norm(&SEED_BLOCKING_DEPS);
        assert!(
            seed.contains(&norm(&blocking_dependency_count("db"))),
            "{seed}"
        );
        assert!(
            !seed.contains(&norm(&fetchable_predicate("dep"))),
            "a dependency whose column lags the predicate is left out of the seed and \
             then subtracted again when its mark lands: {seed}"
        );
    }

    #[test]
    fn the_seed_overwrites_and_never_adjusts() {
        let sql = norm(&SEED_BLOCKING_DEPS);
        assert!(
            sql.starts_with("UPDATE derivation_build db SET blocking_deps = (SELECT count(*)"),
            "{sql}"
        );
        assert!(
            !sql.contains("blocking_deps + ") && !sql.contains("blocking_deps - "),
            "the seed is absolute, not an adjustment: {sql}"
        );
        assert!(
            sql.contains("WHERE db.derivation = ANY($1::uuid[])"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn an_empty_batch_touches_the_database_not_at_all() {
        let db = empty_db();
        let txn = db.begin().await.unwrap();
        let lock = lock_seed_shared_builds(&txn, &[]).await.unwrap();

        assert_eq!(seed_blocking_deps(&lock).await.unwrap(), 0);
        assert!(became_fetchable(&lock).await.unwrap().changes.is_empty());
        assert!(lost_fetchability(&lock).await.unwrap().is_empty());
        txn.commit().await.unwrap();

        assert!(promote(&db, &[]).await.unwrap().is_empty());
        assert!(unpromote_ungated(&db, &[]).await.unwrap().is_empty());
        assert!(statements(db.into_transaction_log()).is_empty());
    }

    #[tokio::test]
    async fn a_flip_and_its_ripple_share_the_locking_transaction() {
        let x = DerivationId::now_v7();
        let d = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([exec(1), exec(0), exec(1)])
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![ripple_row(d, false)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_shared_builds(&txn, &[x]).await.unwrap();
        became_fetchable(&lock).await.unwrap();
        txn.commit().await.unwrap();

        let raw = db.into_transaction_log();
        assert_eq!(raw.len(), 1, "one transaction for the whole flip: {raw:?}");

        let inside: Vec<&str> = raw[0].statements().iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(
            inside.len(),
            10,
            "BEGIN, lock, mark, ripple, then the need update of the marked build in a \
             savepoint (work_mem, lock, walk), COMMIT: {inside:?}"
        );
        assert!(
            inside[1].contains("ORDER BY derivation FOR NO KEY UPDATE"),
            "an unordered locker deadlocks against every ordered one: {inside:?}"
        );
        assert!(inside[2].contains("SET fetchable = true"), "{inside:?}");
        assert!(inside[3].contains("blocking_deps - c.n"), "{inside:?}");
    }

    #[tokio::test]
    async fn became_fetchable_ripples_the_marks_returning_and_promotes_only_zeroes() {
        let flipped = DerivationId::now_v7();
        let untouched = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([exec(2), exec(0), exec(1)])
            .append_query_results([vec![drv(flipped)]])
            .append_query_results([vec![ripple_row(d1, true), ripple_row(d2, false)]])
            .append_query_results([vec![drv(d1)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_shared_builds(&txn, &[flipped, untouched])
            .await
            .unwrap();
        let FetchableStatusChangesAndNewlyMarkedDerivations { changes, marked } =
            became_fetchable(&lock).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(marked, vec![flipped]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].derivation, d1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Created, BuildStatus::Queued)
        );

        let log = statements(db.into_transaction_log());
        assert_eq!(
            log.len(),
            7,
            "lock, mark, ripple, promote, then the need update of the marked build: {log:?}"
        );
        assert!(
            log[1].contains(&flipped.to_string()) && log[1].contains(&untouched.to_string()),
            "the mark is offered both shared builds: {log:?}"
        );
        assert!(
            log[2].contains(&flipped.to_string()) && !log[2].contains(&untouched.to_string()),
            "the ripple takes only the shared build the mark returned: {log:?}"
        );
        assert!(
            log[3].contains(&d1.to_string()) && !log[3].contains(&d2.to_string()),
            "only the parent that reached zero is a candidate: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_shared_build_already_fetchable_moves_nobody() {
        let x = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([exec(1)])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_shared_builds(&txn, &[x]).await.unwrap();
        let changes = became_fetchable(&lock).await.unwrap().changes;
        txn.commit().await.unwrap();

        assert!(changes.is_empty());
        assert_eq!(statements(db.into_transaction_log()).len(), 2, "lock, mark");
    }

    #[tokio::test]
    async fn lost_fetchability_unpromotes_queued_parents_and_reopens_the_walk_below() {
        let x = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([exec(1), exec(2), exec(0), exec(0)])
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![transition_row(d1, 1, 0), transition_row(d2, 0, 0)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_shared_builds(&txn, &[x]).await.unwrap();
        let changes = lost_fetchability(&lock).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].derivation, d1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Queued, BuildStatus::Created)
        );

        let log = statements(db.into_transaction_log());
        assert_eq!(
            log.len(),
            7,
            "lock, mark, parent lock, ripple, then the raised, locked update below the flipped build: {log:?}"
        );
        assert!(
            log[1].contains("SET fetchable = false") && log[1].contains("db.fetchable AND NOT"),
            "{log:?}"
        );
        assert!(
            log[2].contains("ORDER BY p.derivation FOR NO KEY UPDATE")
                && log[2].contains(&x.to_string()),
            "{log:?}"
        );
        assert!(
            log[4].contains("SET LOCAL work_mem")
                && log[5].contains(&x.to_string())
                && log[6].contains("region(evaluation, derivation, builder)"),
            "the update is rooted at the shared build the mark flipped: {log:?}"
        );
        assert!(
            log[3].contains("SET blocking_deps = c.n,")
                && log[3].contains("status = CASE WHEN c.n > 0 AND d.status = 1 AND NOT EXISTS"),
            "{log:?}"
        );
        assert!(
            log[3].contains("updated_at = CASE WHEN c.n > 0 AND d.status = 1 AND NOT EXISTS"),
            "a parent that only counted up keeps its updated_at: a FailedTransient \
             row's retry backoff is measured from that column, so bumping it here \
             restarts the window a dependency's regression had nothing to do with: {log:?}"
        );
    }

    #[test]
    fn a_ripple_up_counts_an_in_flight_shared_build_without_unqueueing_it() {
        let gate = norm(
            &crate::scheduling::assignment_record::no_open_assignment_predicate(
                &crate::scheduling::assignment_record::build_job_key_sql("d.id"),
            ),
        );
        let sql = norm(&BLOCK_PARENTS);
        for column in ["status", "updated_at"] {
            assert!(
                sql.contains(&format!(
                    "{column} = CASE WHEN c.n > 0 AND d.status = 1 AND {gate} THEN"
                )),
                "{sql}"
            );
        }
        assert!(
            sql.contains("SET blocking_deps = c.n,") && sql.contains("AND d.blocking_deps <> c.n"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn blocking_dependency_counter_seed_statement_has_no_recursive_sql_or_recursive_union_plan_without_keyed_cache()
     {
        let fixture = crate::store_path_content_variant_trust::tests::setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
            false,
            |shape, _| shape,
        )
        .await;
        let parent = fixture.parent;
        let blocking_dependency_seed_explain_analyze_output = fixture
            .db
            .texts(&format!(
                "EXPLAIN (ANALYZE) {}",
                SEED_BLOCKING_DEPS.replace("$1::uuid[]", &format!("ARRAY['{parent}']::uuid[]"))
            ))
            .await
            .join("\n");
        fixture.db.finish().await;
        assert_eq!(
            (
                SEED_BLOCKING_DEPS.contains("RECURSIVE"),
                blocking_dependency_seed_explain_analyze_output
                    .contains("Update on derivation_build"),
                blocking_dependency_seed_explain_analyze_output.contains("Recursive Union")
            ),
            (false, true, false),
            "Never follow input closures to determine Readiness. There is no RECURSIVE in the seed statement for the blocking dependency counter. That query plan can update derivation_build without a Recursive Union. {blocking_dependency_seed_explain_analyze_output}"
        );
    }

    #[tokio::test]
    async fn two_concurrent_transactions_mark_different_parent_dependencies_not_fetchable_and_parent_blocking_dependency_count_is_two()
     {
        let [x, y, parent] =
            ["4", "5", "6"].map(|c| format!("/nix/store/{}-race.drv", c.repeat(32)));
        let [x_output, y_output] = ["7", "8"].map(|c| c.repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7())
            .derivation(&x)
            .output(&x_output)
            .build_status(BuildStatus::Completed)
            .cached_path(&x_output, "x")
            .derivation(&y)
            .output(&y_output)
            .build_status(BuildStatus::Completed)
            .cached_path(&y_output, "y")
            .derivation(&parent)
            .depends_on(&x)
            .depends_on(&y)
            .into_database()
            .await;
        db.execute(
            "UPDATE derivation_build SET fetchable = (status = 3), missing_runtime_deps = 0, \
             blocking_deps = 0",
        )
        .await;
        let (x_id, y_id) = (db.id_of(&x).await, db.id_of(&y).await);
        let connection = db.connection();

        let first = connection.begin().await.unwrap();
        first
            .execute_unprepared(&format!(
                "DELETE FROM cached_path WHERE hash = '{x_output}'"
            ))
            .await
            .unwrap();
        lost_fetchability(&lock_shared_builds(&first, &[x_id]).await.unwrap())
            .await
            .unwrap();

        let postgres_lock_waiting_sessions_of_test_database_query = format!(
            "SELECT wait_event_type FROM pg_stat_activity WHERE datname = '{}' AND wait_event_type = 'Lock'",
            db.name()
        );
        let wait_until_second_transaction_blocks_on_row_lock_then_commit_first_transaction = async {
            let mut polls = 0;
            while db
                .texts(&postgres_lock_waiting_sessions_of_test_database_query)
                .await
                .is_empty()
            {
                polls += 1;
                assert!(
                    polls < 200,
                    "The second transaction didn't wait on the parent row that the first uncommitted transaction locked."
                );
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            first.commit().await.unwrap();
        };
        let second = async {
            let second = connection.begin().await.unwrap();
            second
                .execute_unprepared(&format!(
                    "DELETE FROM cached_path WHERE hash = '{y_output}'"
                ))
                .await
                .unwrap();
            lost_fetchability(&lock_shared_builds(&second, &[y_id]).await.unwrap())
                .await
                .unwrap();
            second.commit().await.unwrap();
        };
        tokio::join!(
            second,
            wait_until_second_transaction_blocks_on_row_lock_then_commit_first_transaction
        );

        let blocking_deps = db.shared_build(&parent).await.blocking_deps;
        db.finish().await;
        assert_eq!(
            blocking_deps, 2,
            "Both parent dependencies are no longer fetchable. The parent must wait for two dependencies. A count of 1 can let the parent start with unavailable inputs."
        );
    }
}
