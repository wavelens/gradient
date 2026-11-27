/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::graph::{
    predicates::{gates_predicate, promotable_predicate},
    walks::eval_closure_cte,
};
use crate::sql::StatusWrite;
use crate::status::TransitionChange;

use super::lock::{ids, lock_shared_builds};
use gradient_entity::build::BuildStatus;
use gradient_types::{DerivationId, EvaluationId};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};
use std::sync::LazyLock;

fn promote_sql(scope: &str) -> String {
    format!(
        "UPDATE derivation_build db \
         SET status = {queued}, queued_at = coalesce(db.queued_at, now() AT TIME ZONE 'UTC'), \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE {scope}{promotable} \
         RETURNING db.derivation",
        queued = crate::sql::status::to(BuildStatus::QUEUE),
        promotable = promotable_predicate("db"),
    )
}

static PROMOTE: LazyLock<String> =
    LazyLock::new(|| promote_sql("db.derivation = ANY($1::uuid[]) AND "));

crate::sql_lazy! {
    PROMOTE_QUERY = || PROMOTE.as_str(),
        params = [DerivationIds(64)],
        status_write = StatusWrite::build(BuildStatus::QUEUE);
}

static PROMOTE_ANY: LazyLock<String> =
    LazyLock::new(|| promote_sql("(db.blocking_deps = 0 OR db.cache_available) AND "));

crate::sql_lazy! {
    pub(super) PROMOTE_ANY_QUERY = || PROMOTE_ANY.as_str(),
        params = [],
        tier = Sweep,
        status_write = StatusWrite::build(BuildStatus::QUEUE);
}

static PROMOTE_CLOSURE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{cte} {promote}",
        cte = eval_closure_cte(),
        promote = promote_sql("db.derivation IN (SELECT derivation FROM closure) AND "),
    )
});

crate::sql_lazy! {
    PROMOTE_CLOSURE_QUERY = || PROMOTE_CLOSURE.as_str(),
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(900_000)
            .because("the scope is an evaluation's whole dependency closure, and the \
                      walk that names it reads ~5 buffers per node it visits"),
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::QUEUE);
}

fn unpromote_sql(reason: &str) -> String {
    format!(
        "UPDATE derivation_build db \
         SET status = {created}, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.status = {queued} AND {not_in_flight} AND {reason} \
         RETURNING db.derivation, old.status AS from_status, db.status AS to_status",
        created = crate::sql::status::to(BuildStatus::UNQUEUE),
        queued = crate::sql::status::from(BuildStatus::UNQUEUE),
        not_in_flight = crate::scheduling::assignment_record::no_open_assignment_predicate(
            &crate::scheduling::assignment_record::build_job_key_sql("db.id")
        ),
    )
}

fn unpromote_ungated_sql(scope: &str) -> String {
    unpromote_sql(&format!(
        "{scope}NOT {gates}",
        gates = gates_predicate("db")
    ))
}

static UNPROMOTE_UNGATED: LazyLock<String> = LazyLock::new(|| unpromote_ungated_sql(""));

crate::sql_lazy! {
    pub(super) UNPROMOTE_UNGATED_QUERY = || UNPROMOTE_UNGATED.as_str(),
        params = [],
        tier = Sweep,
        status_write = StatusWrite::build(BuildStatus::UNQUEUE);
}

static UNPROMOTE_UNGATED_IN: LazyLock<String> =
    LazyLock::new(|| unpromote_ungated_sql("db.derivation = ANY($1::uuid[]) AND "));

crate::sql_lazy! {
    UNPROMOTE_UNGATED_IN_QUERY = || UNPROMOTE_UNGATED_IN.as_str(),
        params = [DerivationIds(64)],
        status_write = StatusWrite::build(BuildStatus::UNQUEUE);
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn promote<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(PROMOTE_QUERY.bind([ids(candidates)]))
        .await?;

    Ok(transitions_from(
        returned_derivations(rows),
        BuildStatus::Created,
        BuildStatus::Queued,
    ))
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn promote_closure<C>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &PROMOTE_CLOSURE_QUERY).await?;
    let rows = walk
        .query_all_raw(PROMOTE_CLOSURE_QUERY.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?;
    walk.commit().await?;

    Ok(transitions_from(
        returned_derivations(rows),
        BuildStatus::Created,
        BuildStatus::Queued,
    ))
}

/// The gate is embedded, and the candidate list is a bound, never a claim.
/// A shared build re-walked by a concurrent evaluation is keeping its place in the queue.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn unpromote_ungated<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(UNPROMOTE_UNGATED_IN_QUERY.bind([ids(candidates)]))
            .await?,
    ))
}

/// The un-walk must go first and lock `derivation` rows before `derivation_build`.
/// Record and the GC's orphan reclaim are following the same class order.
pub async fn unwalk_derivations(
    ctx: &crate::DbContext,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let txn = ctx.worker_db.begin().await?;
    crate::graph::walk_completeness::unwalk(&txn, derivations).await?;
    let _shared_builds = lock_shared_builds(&txn, derivations).await?;
    let changes = unpromote_ungated(&txn, derivations).await?;
    txn.commit().await?;

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::test_rows::transition_row;
    use gradient_fixtures::mock::mock_db;

    #[tokio::test]
    async fn unpromote_ungated_moves_queued_rows_back_to_created() {
        let d = DerivationId::now_v7();
        let db = mock_db()
            .append_query_results([vec![transition_row(
                d,
                i32::from(BuildStatus::Queued),
                i32::from(BuildStatus::Created),
            )]])
            .into_connection();

        let changes = unpromote_ungated(&db, &[d]).await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Queued, BuildStatus::Created)
        );
    }
}
