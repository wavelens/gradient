/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::repair::RepairScope;
use crate::graph::{
    predicates::{aborted_in_evaluation, failure_spreads_predicate, settled_incomplete_predicate},
    walks::{
        ClosureDirection, bounded_dependency_closure_cte_body, dependency_closure_cte_body,
        eval_closure_cte, eval_closure_cte_body,
    },
};
use crate::sql::StatusWrite;
use crate::status::TransitionChange;

use super::transitions::returned_transitions;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_types::{DerivationId, EvaluationId};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};

/// A reproducible builder exit must not be thawed by a fresh evaluation.
/// The fleet would otherwise loop through re-queue -> rebuild -> same exit.
fn deterministic_build_failure(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM build_attempt ba WHERE ba.derivation_build = {alias}.id \
         AND ba.outcome = {outcome} AND ba.reason = {reason})",
        outcome = crate::sql::status::attempt_outcome(AttemptOutcome::Failed),
        reason = crate::sql::status::attempt_reason(AttemptFailureReason::BuilderNonzero),
    )
}

/// This statement must run inside [`crate::graph::walks::begin_walk`].
/// The plan gate is proving it with that `work_mem`.
/// A plain connection is planning against the default instead.
pub async fn requeue_failed_shared_builds<C>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut changes = Vec::new();
    for chunk in derivations.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        let walk = crate::graph::walks::begin_walk(db, &REQUEUE_FAILED_SHARED_BUILDS).await?;
        let rows = walk
            .query_all_raw(REQUEUE_FAILED_SHARED_BUILDS.bind([ids.into()]))
            .await?;
        walk.commit().await?;
        changes.extend(returned_transitions(rows));
    }

    Ok(changes)
}

fn requeue_ctes(closure_seed: &str) -> String {
    let deterministic = deterministic_build_failure("dbf");
    format!(
        "WITH RECURSIVE {closure},\n    {blocked}",
        closure =
            dependency_closure_cte_body("closure", closure_seed, ClosureDirection::Dependencies,),
        blocked = bounded_dependency_closure_cte_body(
            "deterministic_blocked",
            &format!(
                "SELECT dbf.derivation FROM derivation_build dbf \
                 WHERE dbf.derivation IN (SELECT derivation FROM closure) AND {deterministic}"
            ),
            ClosureDirection::WantedBy,
            &failure_spreads_predicate("e"),
            Some("closure"),
        ),
    )
}

pub(super) fn requeue_failed_shared_builds_sql() -> String {
    let ctes = requeue_ctes("SELECT unnest($1::uuid[])");
    format!(
        r#"
        {ctes}
        UPDATE derivation_build db
        SET status = {created}, attempt = 0{FORGET_STALE_UPSTREAM_MISS},
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.derivation = ANY($1) AND db.status IN ({requeueable}){SKIP_BLOCKED}
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        created = crate::sql::status::to(BuildStatus::REQUEUE),
        requeueable = crate::sql::status::from(BuildStatus::REQUEUE),
    )
}

crate::sql_fn! {
    REQUEUE_FAILED_SHARED_BUILDS = requeue_failed_shared_builds_sql,
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::REQUEUE);
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn requeue_failed_closure<C>(
    db: &C,
    scope: RepairScope,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let query = match scope {
        RepairScope::Eval(_) => &REQUEUE_FAILED_CLOSURE_FRESH,
        RepairScope::Unstick(_) => &REQUEUE_FAILED_CLOSURE_BLOCKED,
        RepairScope::Retry(_) => &REQUEUE_FAILED_CLOSURE_ALL,
    };
    let walk = crate::graph::walks::begin_walk(db, query).await?;
    let rows = walk
        .query_all_raw(query.bind([Value::Uuid(Some(scope.evaluation().into_inner()))]))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

const FORGET_STALE_UPSTREAM_MISS: &str = ", probed = (db.probed AND (db.cache_available \
     OR COALESCE(db.probed_at > (now() AT TIME ZONE 'UTC') - interval '1 hour', false)))";
const FORGET_UPSTREAM_MISS: &str = ", probed = (db.probed AND db.cache_available)";

/// Drive the write from every failed shared build. A probe per closure member is a second walk.
fn requeue_closure_update(blocked: &str, upstream_answer: &str) -> String {
    format!(
        r#"
        UPDATE derivation_build db
        SET status = {created}, attempt = 0{upstream_answer},
            updated_at = (now() AT TIME ZONE 'UTC')
        FROM (SELECT f.derivation FROM derivation_build f
              WHERE f.status IN ({requeueable})
                AND f.derivation IN (SELECT derivation FROM closure)) failed
        WHERE db.derivation = failed.derivation AND db.status IN ({requeueable}){blocked}
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        created = crate::sql::status::to(BuildStatus::REQUEUE),
        requeueable = crate::sql::status::from(BuildStatus::REQUEUE),
    )
}

/// A task with `retry_failed_builds` off keeps its permanent failures until a user retries them.
fn kept_failed_cte_body() -> String {
    let kept_permanent = format!(
        "SELECT kf.derivation FROM derivation_build kf \
         WHERE kf.status = {failed_permanent} AND kf.derivation IN (SELECT derivation FROM closure) \
           AND EXISTS (SELECT 1 FROM evaluation e JOIN task t ON t.id = e.task \
                       WHERE e.id = $1 AND NOT t.retry_failed_builds)",
        failed_permanent = crate::sql::status::build(BuildStatus::FailedPermanent),
    );
    bounded_dependency_closure_cte_body(
        "kept_failed",
        &format!(
            "SELECT derivation FROM ({aborted} UNION {kept_permanent}) seed",
            aborted = aborted_in_evaluation("$1"),
        ),
        ClosureDirection::WantedBy,
        &failure_spreads_predicate("e"),
        Some("closure"),
    )
}

const SKIP_KEPT: &str =
    "\n          AND NOT EXISTS (SELECT 1 FROM kept_failed x WHERE x.derivation = db.derivation)";
const SKIP_BLOCKED: &str = "\n          AND NOT EXISTS \
     (SELECT 1 FROM deterministic_blocked x WHERE x.derivation = db.derivation)";

fn requeue_failed_closure_fresh_sql() -> String {
    format!(
        "{},\n    {}\n{}",
        eval_closure_cte(),
        kept_failed_cte_body(),
        requeue_closure_update(SKIP_KEPT, FORGET_STALE_UPSTREAM_MISS),
    )
}

fn requeue_failed_closure_all_sql() -> String {
    format!(
        "{}\n{}",
        eval_closure_cte(),
        requeue_closure_update("", FORGET_UPSTREAM_MISS)
    )
}

pub(super) fn requeue_failed_closure_blocked_sql() -> String {
    format!(
        "{},\n    {}\n{}",
        requeue_ctes("SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1"),
        kept_failed_cte_body(),
        requeue_closure_update(
            &format!("{SKIP_BLOCKED}{SKIP_KEPT}"),
            FORGET_STALE_UPSTREAM_MISS,
        ),
    )
}

/// Bypass the deterministic-failure block: the user asked for the rebuild.
fn retry_build_closure_sql() -> String {
    let dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed);
    let retryable = crate::sql::status::build_in(&BuildStatus::RETRYABLE);
    let failed_step = |side: &str, statuses: &str| {
        format!(
            "{spreads} AND EXISTS (SELECT 1 FROM derivation_build p \
             WHERE p.derivation = {side} AND (p.status IN ({statuses}) OR {incomplete}))",
            spreads = failure_spreads_predicate("e"),
            incomplete = settled_incomplete_predicate("p"),
        )
    };
    let causes = bounded_dependency_closure_cte_body(
        "causes",
        "SELECT $2::uuid",
        ClosureDirection::Dependencies,
        &failed_step("e.dependency", &format!("{dependency_failed}, {retryable}")),
        Some("closure"),
    );
    let dependents = bounded_dependency_closure_cte_body(
        "dependents",
        "SELECT derivation FROM retried",
        ClosureDirection::WantedBy,
        &failed_step("e.derivation", &dependency_failed.to_string()),
        Some("closure"),
    );
    format!(
        r#"
        WITH RECURSIVE {closure},
        {causes},
        retried AS (
            SELECT c.derivation FROM causes c
            JOIN derivation_build f ON f.derivation = c.derivation
            WHERE f.status IN ({retryable})
        ),
        {dependents}
        UPDATE derivation_build db
        SET status = {created}, attempt = 0{FORGET_UPSTREAM_MISS},
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.derivation IN (SELECT derivation FROM dependents)
          AND (db.status = {dependency_failed}
               OR (db.status IN ({retryable})
                   AND db.derivation IN (SELECT derivation FROM retried)))
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        closure = eval_closure_cte_body(),
        created = crate::sql::status::to(BuildStatus::REQUEUE),
    )
}

pub async fn retry_build_closure<C>(
    db: &C,
    evaluation: EvaluationId,
    derivation: DerivationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &RETRY_BUILD_CLOSURE).await?;
    let rows = walk
        .query_all_raw(RETRY_BUILD_CLOSURE.bind([
            Value::Uuid(Some(evaluation.into_inner())),
            Value::Uuid(Some(derivation.into_inner())),
        ]))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

pub(super) fn requeue_failed_import_closure_sql() -> String {
    format!(
        "{}\n{}",
        requeue_ctes("SELECT unnest($1::uuid[])"),
        requeue_closure_update(SKIP_BLOCKED, FORGET_STALE_UPSTREAM_MISS),
    )
}

pub async fn requeue_failed_import_closure<C>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut changes = Vec::new();
    for chunk in derivations.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        let walk = crate::graph::walks::begin_walk(db, &REQUEUE_FAILED_IMPORT_CLOSURE).await?;
        let rows = walk
            .query_all_raw(REQUEUE_FAILED_IMPORT_CLOSURE.bind([ids.into()]))
            .await?;
        walk.commit().await?;
        changes.extend(returned_transitions(rows));
    }

    Ok(changes)
}

crate::sql_fn! {
    REQUEUE_FAILED_IMPORT_CLOSURE = requeue_failed_import_closure_sql,
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::REQUEUE);

    REQUEUE_FAILED_CLOSURE_FRESH = requeue_failed_closure_fresh_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, taken only \
                      while the closure holds a failed shared build"),
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::REQUEUE);

    REQUEUE_FAILED_CLOSURE_BLOCKED = requeue_failed_closure_blocked_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, taken only \
                      while the closure holds a failed shared build"),
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::REQUEUE);

    REQUEUE_FAILED_CLOSURE_ALL = requeue_failed_closure_all_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, taken only \
                      while the closure holds a failed shared build"),
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::REQUEUE);

    RETRY_BUILD_CLOSURE = retry_build_closure_sql,
        params = [EvaluationId, DerivationId],
        tier = Walk,
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::REQUEUE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requeue_failed_shared_builds_restores_timed_out_and_dependency_failed_builds_but_keeps_reproducible_failure_and_forgets_stale_cache()
     {
        let path = |c: &str| format!("/nix/store/{}-requeue.drv", c.repeat(32));
        let [
            reproducible,
            above_reproducible,
            timed_out,
            above_timed_out,
            fresh_miss,
            stale_hit,
        ] = ["1", "2", "3", "4", "5", "6"];
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&path(reproducible))
            .walked()
            .job()
            .probed()
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            )
            .derivation(&path(above_reproducible))
            .walked()
            .job()
            .depends_on(&path(reproducible))
            .build_status(BuildStatus::DependencyFailed)
            .derivation(&path(timed_out))
            .walked()
            .job()
            .probed()
            .build_status(BuildStatus::FailedTimeout)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::WallClockTimeout),
            )
            .derivation(&path(above_timed_out))
            .walked()
            .job()
            .depends_on(&path(timed_out))
            .build_status(BuildStatus::DependencyFailed)
            .derivation(&path(fresh_miss))
            .walked()
            .job()
            .probed()
            .build_status(BuildStatus::Aborted)
            .derivation(&path(stale_hit))
            .walked()
            .job()
            .probed()
            .cache_available()
            .build_status(BuildStatus::Aborted)
            .into_database()
            .await;
        let keys = [
            reproducible,
            above_reproducible,
            timed_out,
            above_timed_out,
            fresh_miss,
            stale_hit,
        ];
        let mut ids = Vec::new();
        for key in keys {
            ids.push(db.id_of(&path(key)).await);
        }
        db.execute(&format!(
            "UPDATE derivation_build SET attempt = 3; \
             UPDATE derivation_build SET probed_at = (now() AT TIME ZONE 'UTC') - interval '2 hours' \
             WHERE derivation IN ('{}', '{}')",
            ids[2], ids[5]
        ))
        .await;

        let changes = requeue_failed_shared_builds(&db.connection(), &ids)
            .await
            .unwrap();
        let mut rows = Vec::new();
        for key in keys {
            let row = db.shared_build(&path(key)).await;
            rows.push((row.status, row.attempt, row.probed));
        }
        db.finish().await;

        assert_eq!(changes.len(), 4);
        assert_eq!(
            rows,
            [
                (BuildStatus::FailedPermanent, 3, true),
                (BuildStatus::DependencyFailed, 3, false),
                (BuildStatus::Created, 0, false),
                (BuildStatus::Created, 0, false),
                (BuildStatus::Created, 0, true),
                (BuildStatus::Created, 0, true),
            ]
        );
    }
}
