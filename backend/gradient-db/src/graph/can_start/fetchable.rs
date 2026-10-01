/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::predicates::fetchable_predicate;
use crate::graph::promotion::{returned_derivations, returned_transitions};
use crate::status::TransitionChange;

use super::lock::{SeedLock, SharedBuildLock, ids, lock_shared_builds};
use super::need::{settle_need, update_need};
use super::queue::promote;
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use std::sync::LazyLock;

/// The value `blocking_deps` holds for shared build `{alias}`: its direct build dependencies
/// (`kind IN (0, 2)`) whose shared build row is absent, or present and not startable by
/// `dep_ready`. A runtime-only edge is not an input of the build; `missing_runtime_deps`
/// covers it, and counting it here wedged two derivations that share an output.
///
/// One fragment, so the seed and the recount cannot drift on the part that must not:
/// one count per EDGE, and a `LEFT JOIN` so a dependency with NO shared build row counts as
/// blocking instead of being dropped. An inner join there fails OPEN, in a gate whose
/// entire job is to stop a dispatch against a missing input.
///
/// `dep_ready` is the one thing the two callers differ on, and deliberately: the
/// recount reads the `fetchable` column it has just repaired, the seed evaluates the
/// predicate itself. See [`seed_blocking_deps`].
///
/// Self-edges are NOT excluded, unlike `nar_closure`'s reference count: a store path
/// routinely references itself, a derivation cannot be its own input, and excluding
/// them here would diverge from the frozen backfill for nothing.
pub(super) fn blocking_dependency_count(alias: &str, dep_ready: &str) -> String {
    format!(
        "(SELECT count(*) FROM derivation_dependency e \
         LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
         WHERE e.derivation = {alias}.derivation AND e.kind IN (0, 2) \
           AND (dep.derivation IS NULL OR NOT ({dep_ready})))"
    )
}

pub(super) static SEED_BLOCKING_DEPS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET blocking_deps = {count} \
         WHERE db.derivation = ANY($1::uuid[])",
        count = blocking_dependency_count("db", &fetchable_predicate("dep")),
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

crate::sql! {
    RIPPLE_DOWN = r#"
    UPDATE derivation_build d
    SET blocking_deps = d.blocking_deps - c.n
    FROM (SELECT e.derivation, count(*) AS n FROM derivation_dependency e
          WHERE e.dependency = ANY($1::uuid[]) AND e.kind IN (0, 2) GROUP BY e.derivation) c
    WHERE d.derivation = c.derivation
    RETURNING d.derivation, d.blocking_deps = 0 AS startable
"#,
        params = [DerivationIds(64)],
        tier = Bulk;
}

static RIPPLE_UP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build d \
         SET blocking_deps = d.blocking_deps + c.n, \
             status = CASE WHEN {unqueue} THEN {created} ELSE d.status END, \
             updated_at = CASE WHEN {unqueue} \
                               THEN (now() AT TIME ZONE 'UTC') ELSE d.updated_at END \
         FROM (SELECT e.derivation, count(*) AS n FROM derivation_dependency e \
               WHERE e.dependency = ANY($1::uuid[]) AND e.kind IN (0, 2) \
               GROUP BY e.derivation) c \
         WHERE d.derivation = c.derivation \
         RETURNING d.derivation, old.status AS from_status, d.status AS to_status",
        unqueue = format!(
            "d.status = {queued} AND {not_in_flight}",
            queued = crate::sql::status::build(BuildStatus::Queued),
            not_in_flight = crate::scheduling::assignment_record::no_open_assignment_predicate(
                &crate::scheduling::assignment_record::build_job_key_sql("d.id")
            ),
        ),
        created = crate::sql::status::build(BuildStatus::Created),
    )
});

crate::sql_lazy! {
    RIPPLE_UP_QUERY = || RIPPLE_UP.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

/// Recount, for each locked shared build, the direct dependencies that cannot yet serve
/// their outputs, and write it absolutely. Returns the rows written.
///
/// Run it for a derivation whose edges just landed, in the transaction that wrote
/// them, which is what the lock proof requires: the count is over
/// `derivation_dependency`, so an edge inserted after the seed is one a later ripple
/// can cancel without it ever having been counted. It overwrites rather than adjusts
/// on purpose: the value it replaces was counted over an older edge set, or is the
/// column default on a row a retire has just reset, and an adjustment would carry
/// that error forward instead of ending it.
///
/// It evaluates [`crate::graph::predicates::fetchable_predicate`] on each dependency rather
/// than reading the `fetchable` column, because this is the first reader of a
/// dependency's can-start state and it executes strictly before any sweep could have corrected
/// a stale flag. The module doc has the full argument and the cost.
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

/// Flip the locked shared builds to fetchable where the predicate now holds, decrement their
/// direct parents' counters, and queue the parents that reached zero.
///
/// The returned transitions are `Created` to `Queued`, and the ripple moves only over
/// the rows the mark actually flipped: a caller that hands over a shared build that was
/// already fetchable gets no statement past the mark, which is what keeps a parent's
/// counter from going below zero.
pub async fn became_fetchable(lock: &SharedBuildLock<'_>) -> Result<Vec<TransitionChange>, DbErr> {
    let flipped = mark(lock, true).await?;
    if flipped.is_empty() {
        return Ok(Vec::new());
    }

    let rows = lock
        .txn
        .query_all_raw(RIPPLE_DOWN.bind([ids(&flipped)]))
        .await?;

    let mut startable = Vec::new();
    for row in rows {
        if row.try_get::<bool>("", "startable")? {
            startable.push(DerivationId::new(
                row.try_get::<uuid::Uuid>("", "derivation")?,
            ));
        }
    }

    promote(lock.txn, &startable).await
}

/// [`became_fetchable`] with the transaction and the lock it needs, for an event
/// that names its shared builds and does nothing else to them.
///
/// `begin` is a real transaction on a pooled handle and a SAVEPOINT on one that
/// already stands for a transaction, and a savepoint's locks are held to the OUTER
/// commit, so this one shape is correct inside the graph writer and outside it. The
/// caller emits the returned transitions after this returns and never between the
/// lock and the commit, which is why they are returned rather than emitted here.
pub async fn advance_fetchable<C>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let txn = db.begin().await?;
    let lock = lock_shared_builds(&txn, derivations).await?;
    let changes = became_fetchable(&lock).await?;
    txn.commit().await?;

    Ok(changes)
}

/// Flip the locked shared builds to not fetchable where the predicate no longer holds,
/// increment their direct parents' counters, and pull the queued parents back to
/// `Created`; a parent that was `Created`, `Building` or terminal, or whose job is
/// already in flight, only counts up.
///
/// A shared build that stops being fetchable is open again, so the walk below it is
/// re-opened here too: the need flag is updated from the flipped shared builds and the queue
/// settled against it, in the flip's own transaction. A `Completed` shared build whose
/// closure just lost a path is the only way the missing dependency below it is reached, and a
/// retire that dropped the flag and asked for nothing left 47 builders waiting
/// behind 22 such shared builds.
pub async fn lost_fetchability(lock: &SharedBuildLock<'_>) -> Result<Vec<TransitionChange>, DbErr> {
    let flipped = mark(lock, false).await?;
    if flipped.is_empty() {
        return Ok(Vec::new());
    }

    let rows = lock
        .txn
        .query_all_raw(RIPPLE_UP_QUERY.bind([ids(&flipped)]))
        .await?;
    let mut changes: Vec<TransitionChange> = returned_transitions(rows)
        .into_iter()
        .filter(|c| c.from != c.to)
        .collect();

    let moved = update_need(lock.txn, &flipped).await?;
    changes.extend(settle_need(lock.txn, &moved).await?);

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::lock::lock_seed_shared_builds;
    use crate::graph::can_start::queue::{unpromote_drv_owners, unpromote_ungated};
    use crate::graph::can_start::repair::RECOUNT_BLOCKING_DEPS;
    use crate::graph::can_start::test_rows::{drv, exec, norm, transition_row};
    use crate::pool::statements;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
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
            RIPPLE_DOWN.text().into_owned(),
            RIPPLE_UP.to_string(),
        ] {
            assert!(sql.contains("e.kind IN (0, 2)"), "{sql}");
        }
    }

    /// A dependency with no shared build row at all must count as BLOCKING. An inner join
    /// counted zero and failed open, in a gate whose whole job is to stop a dispatch
    /// against a missing input.
    #[test]
    fn the_count_treats_a_dependency_with_no_shared_build_as_blocking() {
        let sql = norm(&blocking_dependency_count("db", "dep.fetchable"));
        assert_eq!(
            sql,
            "(SELECT count(*) FROM derivation_dependency e \
             LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
             WHERE e.derivation = db.derivation \
             AND e.kind IN (0, 2) \
             AND (dep.derivation IS NULL OR NOT (dep.fetchable)))"
        );
    }

    /// The seed writes the count absolutely, so it never accumulates onto - or trusts
    /// - the value it finds, and a leaf gets zero from an empty count.
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

    /// An empty batch is not a statement, the lock included: every entry point
    /// short-circuits so a caller can hand over whatever its event produced.
    #[tokio::test]
    async fn an_empty_batch_touches_the_database_not_at_all() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let txn = db.begin().await.unwrap();
        let lock = lock_seed_shared_builds(&txn, &[]).await.unwrap();

        assert_eq!(seed_blocking_deps(&lock).await.unwrap(), 0);
        assert!(became_fetchable(&lock).await.unwrap().is_empty());
        assert!(lost_fetchability(&lock).await.unwrap().is_empty());
        txn.commit().await.unwrap();

        assert!(promote(&db, &[]).await.unwrap().is_empty());
        assert!(unpromote_drv_owners(&db, &[]).await.unwrap().is_empty());
        assert!(unpromote_ungated(&db, &[]).await.unwrap().is_empty());
        assert!(statements(db.into_transaction_log()).is_empty());
    }

    /// The mark and its compensating ripple must land or roll back together: on a
    /// pooled handle a killed ripple leaves the flip committed and the counter move
    /// gone, and the retry marks nothing so it ripples nothing. The lock proof is what
    /// makes that impossible to write, so it must be the only way in.
    #[tokio::test]
    async fn a_flip_and_its_ripple_share_the_locking_transaction() {
        let x = DerivationId::now_v7();
        let d = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1)])
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![ripple_row(d, false)]])
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
            5,
            "BEGIN, lock, mark, ripple, COMMIT: {inside:?}"
        );
        assert!(
            inside[1].contains("ORDER BY derivation FOR NO KEY UPDATE"),
            "an unordered locker deadlocks against every ordered one: {inside:?}"
        );
        assert!(inside[2].contains("SET fetchable = true"), "{inside:?}");
        assert!(inside[3].contains("blocking_deps - c.n"), "{inside:?}");
    }

    /// Becoming fetchable decrements every direct parent once per edge and promotes
    /// only the parents that reached zero and pass the gates. Two shared builds go in and
    /// one flips, so an implementation that rippled the caller's list instead of the
    /// mark's `RETURNING` writes the wrong frontier and fails here - which is the
    /// regression the module doc calls unrecoverable.
    #[tokio::test]
    async fn became_fetchable_ripples_the_marks_returning_and_promotes_only_zeroes() {
        let flipped = DerivationId::now_v7();
        let untouched = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(2)])
            .append_query_results([vec![drv(flipped)]])
            .append_query_results([vec![ripple_row(d1, true), ripple_row(d2, false)]])
            .append_query_results([vec![drv(d1)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_shared_builds(&txn, &[flipped, untouched])
            .await
            .unwrap();
        let changes = became_fetchable(&lock).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].derivation, d1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Created, BuildStatus::Queued)
        );

        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 4, "lock, mark, ripple, promote: {log:?}");
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

    /// A flip that changed nothing ripples nothing: the mark returns no row, and no
    /// further statement executes. Rippling a state instead of a transition is what drives
    /// a parent's counter below zero, where no gate reads it again.
    #[tokio::test]
    async fn a_shared_build_already_fetchable_moves_nobody() {
        let x = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1)])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_shared_builds(&txn, &[x]).await.unwrap();
        let changes = became_fetchable(&lock).await.unwrap();
        txn.commit().await.unwrap();

        assert!(changes.is_empty());
        assert_eq!(statements(db.into_transaction_log()).len(), 2, "lock, mark");
    }

    /// Losing fetchability increments every direct parent and pulls the queued ones
    /// back to Created; a Created or Building parent only counts up. Then the walk
    /// below the flipped shared build is re-opened: the need update starts from exactly
    /// the rows the mark returned, under its own raise, in the flip's transaction.
    #[tokio::test]
    async fn lost_fetchability_unpromotes_queued_parents_and_reopens_the_walk_below() {
        let x = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1), exec(0), exec(0)])
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
            6,
            "lock, mark, ripple, then the raised, locked update below the flip: {log:?}"
        );
        assert!(
            log[1].contains("SET fetchable = false") && log[1].contains("db.fetchable AND NOT"),
            "{log:?}"
        );
        assert!(
            log[3].contains("SET LOCAL work_mem")
                && log[4].contains(&x.to_string())
                && log[5].contains("region(evaluation, derivation, builder)"),
            "the update is rooted at the shared build the mark flipped: {log:?}"
        );
        assert!(
            log[2].contains("blocking_deps + c.n")
                && log[2].contains("status = CASE WHEN d.status = 1 AND NOT EXISTS"),
            "{log:?}"
        );
        assert!(
            log[2].contains("updated_at = CASE WHEN d.status = 1 AND NOT EXISTS"),
            "a parent that only counted up keeps its updated_at: a FailedTransient \
             row's retry backoff is measured from that column, so bumping it here \
             restarts the window a dependency's regression had nothing to do with: {log:?}"
        );
    }

    /// The ripple still counts the lost dependency on a shared build whose job is in
    /// flight, but only the un-promotes' reason may move its status.
    #[test]
    fn a_ripple_up_counts_an_in_flight_shared_build_without_unqueueing_it() {
        let gate = norm(
            &crate::scheduling::assignment_record::no_open_assignment_predicate(
                &crate::scheduling::assignment_record::build_job_key_sql("d.id"),
            ),
        );
        let sql = norm(&RIPPLE_UP);
        for column in ["status", "updated_at"] {
            assert!(
                sql.contains(&format!(
                    "{column} = CASE WHEN d.status = 1 AND {gate} THEN"
                )),
                "{sql}"
            );
        }
        assert!(
            sql.contains("blocking_deps = d.blocking_deps + c.n"),
            "{sql}"
        );
    }
}
