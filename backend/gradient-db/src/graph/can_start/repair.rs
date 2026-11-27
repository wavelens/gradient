/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::status::TransitionChange;

use super::fetchable::{blocking_dependency_count, mark_fetchable, mark_unfetchable};
use super::lock::{SharedBuildLock, ids, try_lock_shared_builds};
use super::queue::{PROMOTE_ANY_QUERY, UNPROMOTE_UNGATED_QUERY};
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use std::sync::LazyLock;
use tracing::debug;

pub const SWEEP_LOCK_CHUNK: usize = 1000;
pub const SWEEP_CHUNKS_PER_TRANSACTION: usize = 8;

fn repair_scope() -> String {
    let pending = crate::sql::status::build_in(&BuildStatus::PENDING);
    let terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS);
    format!(
        "SELECT q.derivation FROM derivation_build q WHERE q.status IN ({pending}) \
       UNION \
         SELECT e.dependency FROM derivation_dependency e \
         JOIN derivation_build q ON q.derivation = e.derivation \
         WHERE q.status IN ({pending}) \
       UNION \
         SELECT q.derivation FROM derivation_build q \
         WHERE q.fetchable AND q.missing_runtime_deps > 0 \
       UNION \
         SELECT q.derivation FROM derivation_build q \
         WHERE q.status IN ({terminal_success}) AND NOT q.fetchable"
    )
}

crate::sql_fn! {
    REPAIR_SCOPE_QUERY = repair_scope,
        params = [],
        tier = Sweep;
}

pub(super) static RECOUNT_BLOCKING_DEPS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET blocking_deps = x.n \
         FROM (SELECT p.derivation, p.blocking_deps AS old, {count} AS n \
               FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])) x \
         WHERE db.derivation = x.derivation AND db.blocking_deps = x.old AND x.old <> x.n",
        count = blocking_dependency_count("p"),
    )
});

crate::sql_lazy! {
    RECOUNT_BLOCKING_DEPS_QUERY = || RECOUNT_BLOCKING_DEPS.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

#[derive(Debug, Default)]
pub struct RepairedFetchable {
    pub marked: u64,
    pub unqueued: Vec<TransitionChange>,
}

#[derive(Debug, Default)]
pub struct UnpromotedAndPromotedStatusTransitions {
    pub promoted: Vec<TransitionChange>,
    pub unpromoted: Vec<TransitionChange>,
}

pub async fn can_start_scope<C: ConnectionTrait>(db: &C) -> Result<Vec<DerivationId>, DbErr> {
    db.query_all_raw(REPAIR_SCOPE_QUERY.stmt())
        .await?
        .into_iter()
        .map(|r| {
            r.try_get::<uuid::Uuid>("", "derivation")
                .map(DerivationId::new)
        })
        .collect()
}

async fn recount_blocking_deps(lock: &SharedBuildLock<'_>) -> Result<u64, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(0);
    }

    Ok(lock
        .txn
        .execute_raw(RECOUNT_BLOCKING_DEPS_QUERY.bind([ids(&lock.derivations)]))
        .await?
        .rows_affected())
}

async fn skip_held_chunk(txn: DatabaseTransaction, shared_builds: usize) -> Result<(), DbErr> {
    txn.rollback().await?;
    debug!(
        shared_builds,
        "consistency check skipped a chunk the graph writer holds"
    );
    Ok(())
}

/// A column corrected without adjusting its parents' `blocking_deps` leaves them off by one for good.
/// Items that reached zero enter the queue in [`repair_all_shared_build_start_gate_states`] after all counters are recomputed.
pub async fn repair_fetchable<C>(db: &C, scope: &[DerivationId]) -> Result<RepairedFetchable, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut repaired = RepairedFetchable::default();
    for chunk in scope.chunks(SWEEP_LOCK_CHUNK) {
        let txn = db.begin().await?;
        let Some(lock) = try_lock_shared_builds(&txn, chunk).await? else {
            skip_held_chunk(txn, chunk.len()).await?;
            continue;
        };
        let fetchable = mark_fetchable(&lock).await?;
        let unfetchable = mark_unfetchable(&lock).await?;
        txn.commit().await?;

        repaired.marked += (fetchable.marked.len() + unfetchable.marked.len()) as u64;
        repaired.unqueued.extend(unfetchable.unqueued);
    }

    Ok(repaired)
}

pub async fn recount_blocking_deps_for_derivation_ids<C>(
    db: &C,
    scope: &[DerivationId],
) -> Result<u64, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut blocking_deps = 0u64;
    for chunk in scope.chunks(SWEEP_LOCK_CHUNK) {
        let txn = db.begin().await?;
        let Some(lock) = try_lock_shared_builds(&txn, chunk).await? else {
            skip_held_chunk(txn, chunk.len()).await?;
            continue;
        };
        blocking_deps += recount_blocking_deps(&lock).await?;
        txn.commit().await?;
    }

    Ok(blocking_deps)
}

/// The un-promote is going first.
/// EvalPlanQual is not re-evaluating the gate's `EXISTS` subqueries.
/// A promote can still fire on a `.drv` retired during a lock wait.
pub async fn repair_all_shared_build_start_gate_states<C: ConnectionTrait>(
    db: &C,
) -> Result<UnpromotedAndPromotedStatusTransitions, DbErr> {
    let unpromoted = returned_transitions(db.query_all_raw(UNPROMOTE_UNGATED_QUERY.stmt()).await?);
    let promoted = transitions_from(
        returned_derivations(db.query_all_raw(PROMOTE_ANY_QUERY.stmt()).await?),
        BuildStatus::Created,
        BuildStatus::Queued,
    );

    Ok(UnpromotedAndPromotedStatusTransitions {
        promoted,
        unpromoted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::test_rows::{drv, exec, transition_row};
    use gradient_fixtures::mock::mock_db;
    use sea_orm::Value;
    use std::collections::BTreeMap;

    fn startable_row(id: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("startable".to_owned(), Value::from(true)),
        ])
    }

    fn locked() -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([(
            "?column?".to_owned(),
            Value::Int(Some(1)),
        )])]
    }

    #[tokio::test]
    async fn repair_fetchable_keeps_fetchable_build_with_stored_output_copy_and_marks_fetchable_build_without_stored_output_copy_not_fetchable_with_waiting_parent()
     {
        let [
            path_of_completed_build_with_stored_output_copy,
            completed_build_path_without_stored_output,
            store_path_of_pending_parent_build_depending_on_both_completed_builds,
        ] = ["4", "5", "6"].map(|c| format!("/nix/store/{}-repair.drv", c.repeat(32)));
        let [
            completed_build_output_path_hash_with_stored_copy,
            completed_build_output_path_hash_without_stored_copy,
        ] = ["7", "8"].map(|c| c.repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7())
            .derivation(&path_of_completed_build_with_stored_output_copy)
            .output(&completed_build_output_path_hash_with_stored_copy)
            .build_status(BuildStatus::Completed)
            .cached_path(
                &completed_build_output_path_hash_with_stored_copy,
                "with-copy",
            )
            .derivation(&completed_build_path_without_stored_output)
            .output(&completed_build_output_path_hash_without_stored_copy)
            .build_status(BuildStatus::Completed)
            .derivation(&store_path_of_pending_parent_build_depending_on_both_completed_builds)
            .depends_on(&path_of_completed_build_with_stored_output_copy)
            .depends_on(&completed_build_path_without_stored_output)
            .into_database()
            .await;
        db.execute(
            "UPDATE derivation_build SET fetchable = true, missing_runtime_deps = 0 WHERE status = 3",
        )
        .await;
        let connection = db.connection();

        let scope = can_start_scope(&connection)
            .await
            .expect("Repair scope read.");
        repair_fetchable(&connection, &scope)
            .await
            .expect("Scheduled repair ran.");
        let fetchable_flags_of_completed_builds_after_repair = (
            db.shared_build(&path_of_completed_build_with_stored_output_copy)
                .await
                .fetchable,
            db.shared_build(&completed_build_path_without_stored_output)
                .await
                .fetchable,
        );

        assert_eq!(
            fetchable_flags_of_completed_builds_after_repair,
            (true, false),
            "Completed builds with a copy in the output remain fetchable. Completed builds without a copy in the output receive a “not fetchable” mark."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn repair_fetchable_reports_marked_count_and_unqueued_parent() {
        let now_fetchable = DerivationId::now_v7();
        let unblocked_parent = DerivationId::now_v7();
        let no_longer_fetchable = DerivationId::now_v7();
        let unqueued_parent = DerivationId::now_v7();
        let db = mock_db()
            .append_query_results([locked()])
            .append_query_results([vec![drv(now_fetchable)]])
            .append_query_results([vec![startable_row(unblocked_parent)]])
            .append_query_results([vec![drv(no_longer_fetchable)]])
            .append_query_results([vec![transition_row(unqueued_parent, 1, 0)]])
            .append_exec_results([exec(1)])
            .into_connection();

        let repaired = repair_fetchable(&db, &[now_fetchable, no_longer_fetchable])
            .await
            .unwrap();

        assert_eq!(repaired.marked, 2);
        assert_eq!(repaired.unqueued.len(), 1);
        assert_eq!(repaired.unqueued[0].derivation, unqueued_parent);
        assert_eq!(
            (repaired.unqueued[0].from, repaired.unqueued[0].to),
            (BuildStatus::Queued, BuildStatus::Created)
        );
    }

    #[tokio::test]
    async fn full_repair_pass_over_one_scope_recounts_three_blocking_deps_and_promotes_one_build() {
        let a = DerivationId::now_v7();
        let demoted = DerivationId::now_v7();
        let promoted = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new;
        let db = mock_db()
            .append_query_results([vec![drv(a)]])
            .append_query_results([locked(), empty(), empty(), locked()])
            .append_exec_results([exec(3)])
            .append_query_results([vec![transition_row(demoted, 1, 0)]])
            .append_query_results([vec![drv(promoted)]])
            .into_connection();

        let scope = can_start_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let blocking_deps = recount_blocking_deps_for_derivation_ids(&db, &scope)
            .await
            .unwrap();
        let queue = repair_all_shared_build_start_gate_states(&db)
            .await
            .unwrap();

        assert_eq!((fetchable.marked, blocking_deps), (0, 3));
        assert_eq!(queue.unpromoted.len(), 1);
        assert_eq!(queue.unpromoted[0].derivation, demoted);
        assert_eq!(queue.promoted.len(), 1);
        assert_eq!(queue.promoted[0].derivation, promoted);
    }

    #[tokio::test]
    async fn a_chunk_the_graph_writer_holds_is_skipped_until_the_next_pass() {
        let a = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new;
        let db = mock_db()
            .append_query_results([vec![drv(a)]])
            .append_query_results([empty(), locked()])
            .append_exec_results([exec(1)])
            .into_connection();

        let scope = can_start_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let blocking_deps = recount_blocking_deps_for_derivation_ids(&db, &scope)
            .await
            .unwrap();

        assert_eq!((fetchable.marked, blocking_deps), (0, 1));
    }

    #[tokio::test]
    async fn repair_recounts_one_row_per_lock_chunk_and_marks_none_fetchable_with_more_derivations_than_one_chunk()
     {
        let scope: Vec<BTreeMap<String, Value>> = (0..SWEEP_LOCK_CHUNK + 1)
            .map(|_| drv(DerivationId::now_v7()))
            .collect();
        let empty = Vec::<BTreeMap<String, Value>>::new;
        let db = mock_db()
            .append_query_results([scope])
            .append_query_results([locked(), empty(), empty(), locked(), empty(), empty()])
            .append_query_results([locked(), locked()])
            .append_exec_results(vec![exec(1); 2])
            .append_query_results([empty(), empty()])
            .into_connection();

        let scope = can_start_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let blocking_deps = recount_blocking_deps_for_derivation_ids(&db, &scope)
            .await
            .unwrap();
        repair_all_shared_build_start_gate_states(&db)
            .await
            .unwrap();

        assert_eq!(
            (fetchable.marked, blocking_deps),
            (0, 2),
            "one recounted row per chunk"
        );
    }

    #[tokio::test]
    async fn repair_scope_query_returns_pending_builds_direct_dependencies_fetchable_builds_with_missing_runtime_deps_and_terminal_success_builds_not_flagged_fetchable_but_no_other_builds()
     {
        let path = |c: &str| format!("/nix/store/{}-scope.drv", c.repeat(32));
        let [
            pending,
            dependency,
            grandparent,
            queued,
            missing_runtime,
            unflagged,
            settled,
            failed,
        ] = ["1", "2", "3", "4", "5", "6", "7", "8"];
        let completed = |shape: gradient_fixtures::shape::Shape, key: &str| {
            shape
                .derivation(&path(key))
                .output(&key.repeat(32))
                .build_status(BuildStatus::Completed)
                .cached_path(&key.repeat(32), key)
        };
        let shape = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7());
        let shape = [dependency, missing_runtime, unflagged, settled]
            .into_iter()
            .fold(shape, completed);
        let db = shape
            .derivation(&path(pending))
            .depends_on(&path(dependency))
            .derivation(&path(queued))
            .build_status(BuildStatus::Queued)
            .derivation(&path(failed))
            .build_status(BuildStatus::FailedPermanent)
            .derivation(&path(grandparent))
            .depends_on(&path(pending))
            .build_status(BuildStatus::Completed)
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE derivation_build SET fetchable = (status = 3), missing_runtime_deps = 0; \
             UPDATE derivation_build SET missing_runtime_deps = 2 \
               WHERE derivation = '{}'; \
             UPDATE derivation_build SET fetchable = false WHERE derivation = '{}'",
            db.id_of(&path(missing_runtime)).await,
            db.id_of(&path(unflagged)).await,
        ))
        .await;

        let scope: std::collections::BTreeSet<_> = can_start_scope(&db.connection())
            .await
            .unwrap()
            .into_iter()
            .collect();
        let mut expected = std::collections::BTreeSet::new();
        for key in [pending, dependency, queued, missing_runtime, unflagged] {
            expected.insert(db.id_of(&path(key)).await);
        }
        db.finish().await;

        assert_eq!(scope, expected);
    }
}
