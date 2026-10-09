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

/// A `LEFT JOIN` is counting a dependency with no shared build row as blocking.
/// An inner join would fail open, in a gate meant to stop a dispatch against a missing input.
/// Runtime-only edges are not build inputs, and `missing_runtime_deps` is covering them.
pub(super) fn blocking_dependency_count(alias: &str) -> String {
    format!(
        "(SELECT count(*) FROM derivation_dependency e \
         LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
         WHERE e.derivation = {alias}.derivation AND e.kind IN (0, 2) \
           AND (dep.derivation IS NULL OR NOT dep.fetchable))"
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

crate::sql! {
    UNBLOCK_PARENTS = r#"
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

static BLOCK_PARENTS: LazyLock<String> = LazyLock::new(|| {
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
    BLOCK_PARENTS_QUERY = || BLOCK_PARENTS.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
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
    pub(super) marked: usize,
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

    Ok(MarkedFetchable {
        marked: marked.len(),
        startable,
    })
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

#[tracing::instrument(level = "debug", skip_all)]
pub async fn became_fetchable(lock: &SharedBuildLock<'_>) -> Result<Vec<TransitionChange>, DbErr> {
    let marked = mark_fetchable(lock).await?;
    promote(lock.txn, &marked.startable).await
}

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
            "(SELECT count(*) FROM derivation_dependency e \
             LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
             WHERE e.derivation = db.derivation \
             AND e.kind IN (0, 2) \
             AND (dep.derivation IS NULL OR NOT dep.fetchable))"
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
