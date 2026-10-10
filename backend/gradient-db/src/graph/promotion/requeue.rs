/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::repair::RepairScope;
use crate::graph::{
    predicates::{aborted_in_evaluation, non_passthrough_predicate},
    walks::{
        ClosureDirection, bounded_dependency_closure_cte_body, dependency_closure_cte_body,
        eval_closure_cte, eval_closure_cte_body,
    },
};
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
            &non_passthrough_predicate("e.derivation"),
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
        SET status = {created}, attempt = 0,
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.derivation = ANY($1) AND db.status IN ({requeueable})
          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        created = crate::sql::status::build(BuildStatus::Created),
        requeueable = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE),
    )
}

crate::sql_fn! {
    REQUEUE_FAILED_SHARED_BUILDS = requeue_failed_shared_builds_sql,
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

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

const KEEP_UPSTREAM_ANSWER: &str = "";
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
        created = crate::sql::status::build(BuildStatus::Created),
        requeueable = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE),
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
        &non_passthrough_predicate("e.derivation"),
        Some("closure"),
    )
}

const SKIP_KEPT: &str = "\n          AND db.derivation NOT IN (SELECT derivation FROM kept_failed)";

fn requeue_failed_closure_fresh_sql() -> String {
    format!(
        "{},\n    {}\n{}",
        eval_closure_cte(),
        kept_failed_cte_body(),
        requeue_closure_update(SKIP_KEPT, KEEP_UPSTREAM_ANSWER),
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
            &format!(
                "\n          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked){SKIP_KEPT}"
            ),
            KEEP_UPSTREAM_ANSWER,
        ),
    )
}

/// Bypass the deterministic-failure block: the user asked for the rebuild.
fn retry_build_closure_sql() -> String {
    let dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed);
    let retryable = crate::sql::status::build_in(&BuildStatus::RETRYABLE);
    let failed_step = |side: &str, statuses: &str| {
        format!(
            "{} AND EXISTS (SELECT 1 FROM derivation_build p \
             WHERE p.derivation = {side} AND p.status IN ({statuses}))",
            non_passthrough_predicate(side),
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
        created = crate::sql::status::build(BuildStatus::Created),
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
        requeue_closure_update(
            "\n          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)",
            KEEP_UPSTREAM_ANSWER,
        ),
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
        flags = [Walk];

    REQUEUE_FAILED_CLOSURE_FRESH = requeue_failed_closure_fresh_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, taken only \
                      while the closure holds a failed shared build"),
        flags = [Walk];

    REQUEUE_FAILED_CLOSURE_BLOCKED = requeue_failed_closure_blocked_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, taken only \
                      while the closure holds a failed shared build"),
        flags = [Walk];

    REQUEUE_FAILED_CLOSURE_ALL = requeue_failed_closure_all_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, taken only \
                      while the closure holds a failed shared build"),
        flags = [Walk];

    RETRY_BUILD_CLOSURE = retry_build_closure_sql,
        params = [EvaluationId, DerivationId],
        tier = Walk,
        flags = [Walk];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_build_failure_predicate_matches_builder_nonzero() {
        let sql = deterministic_build_failure("db")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            sql,
            format!(
                "EXISTS (SELECT 1 FROM build_attempt ba WHERE ba.derivation_build = db.id \
                 AND ba.outcome = {outcome} AND ba.reason = {reason})",
                outcome = crate::sql::status::attempt_outcome(AttemptOutcome::Failed),
                reason = crate::sql::status::attempt_reason(AttemptFailureReason::BuilderNonzero),
            ),
        );
    }

    #[test]
    fn a_fresh_evaluation_thaws_every_failure_in_its_closure() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(requeue_failed_closure_fresh_sql());
        assert!(
            sql.starts_with("WITH RECURSIVE closure(derivation) AS"),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "db.status IN ({}) AND db.derivation NOT IN (SELECT derivation FROM kept_failed) RETURNING",
                crate::sql::status::build_in(&BuildStatus::REQUEUEABLE)
            )),
            "{sql}"
        );
        assert!(
            !sql.contains("deterministic_blocked") && !sql.contains("build_attempt"),
            "a reproducible failure is retried once per evaluation: {sql}"
        );
    }

    #[test]
    fn no_heal_of_an_evaluation_thaws_what_its_user_aborted_or_its_task_keeps() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let aborted = norm(aborted_in_evaluation("$1"));
        for sql in [
            norm(requeue_failed_closure_fresh_sql()),
            norm(requeue_failed_closure_blocked_sql()),
        ] {
            assert!(
                sql.contains(&format!(
                    "kept_failed(derivation) AS (SELECT derivation FROM ({aborted} UNION"
                )),
                "{sql}"
            );
            assert!(
                sql.contains("WHERE e.id = $1 AND NOT t.retry_failed_builds"),
                "{sql}"
            );
            assert!(
                sql.contains(
                    "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
                ),
                "the dependents of the aborted build stay failed: {sql}"
            );
            assert!(
                sql.contains("db.derivation NOT IN (SELECT derivation FROM kept_failed)"),
                "{sql}"
            );
        }
    }

    #[test]
    fn a_user_retry_of_an_evaluation_thaws_all_failures_in_its_closure() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(requeue_failed_closure_all_sql());
        let requeueable = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE);
        assert!(
            sql.contains(&format!("db.status IN ({requeueable}) RETURNING")),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "FROM (SELECT f.derivation FROM derivation_build f WHERE f.status IN ({requeueable}) \
                 AND f.derivation IN (SELECT derivation FROM closure)) failed"
            )),
            "the failed shared builds drive the write, not the closure: {sql}"
        );
        assert!(
            !sql.contains("kept_failed") && !sql.contains("deterministic_blocked"),
            "{sql}"
        );
    }

    #[test]
    fn only_a_user_retry_asks_the_upstream_caches_again() {
        let forgets_the_miss = "attempt = 0, probed = (db.probed AND db.cache_available),";
        for sql in [requeue_failed_closure_all_sql(), retry_build_closure_sql()] {
            let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(sql.contains(forgets_the_miss), "{sql}");
        }

        for sql in [
            requeue_failed_closure_fresh_sql(),
            requeue_failed_closure_blocked_sql(),
            requeue_failed_shared_builds_sql(),
            requeue_failed_import_closure_sql(),
        ] {
            assert!(!sql.contains("probed"), "{sql}");
        }
    }

    #[test]
    fn a_retry_thaws_the_failed_causes_and_only_the_dependents_they_failed() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(retry_build_closure_sql());
        let dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed);
        let retryable = crate::sql::status::build_in(&BuildStatus::RETRYABLE);
        assert!(
            sql.starts_with("WITH RECURSIVE closure(derivation) AS"),
            "the walk stays inside the evaluation: {sql}"
        );
        assert!(
            sql.contains("causes(derivation) AS (SELECT $2::uuid UNION"),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "WHERE p.derivation = e.dependency AND p.status IN ({dependency_failed}, {retryable})"
            )),
            "the walk down passes failed dependencies only: {sql}"
        );
        assert!(
            sql.contains(&format!(
                "retried AS ( SELECT c.derivation FROM causes c \
                 JOIN derivation_build f ON f.derivation = c.derivation \
                 WHERE f.status IN ({retryable}) )"
            )),
            "{sql}"
        );
        assert!(
            sql.contains("dependents(derivation) AS (SELECT derivation FROM retried UNION"),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "WHERE p.derivation = e.derivation AND p.status IN ({dependency_failed})"
            )),
            "the walk up stops at a parent that failed on its own: {sql}"
        );
        assert!(
            sql.contains(&format!(
                "AND (db.status = {dependency_failed} OR (db.status IN ({retryable}) \
                 AND db.derivation IN (SELECT derivation FROM retried)))"
            )),
            "{sql}"
        );
        assert!(
            !sql.contains("deterministic_blocked") && !sql.contains("build_attempt"),
            "a retry rebuilds a reproducible failure on purpose: {sql}"
        );
    }

    #[test]
    fn requeue_excludes_the_deterministic_blocked_subtree() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let deterministic = norm(deterministic_build_failure("dbf"));
        for sql in [
            norm(requeue_failed_shared_builds_sql()),
            norm(requeue_failed_closure_blocked_sql()),
            norm(requeue_failed_import_closure_sql()),
        ] {
            assert!(
                sql.contains("deterministic_blocked(derivation) AS"),
                "must build the deterministic-blocked closure: {sql}"
            );
            assert!(
                sql.contains(&deterministic),
                "blocked set must seed from a reproducible builder-nonzero exit: {sql}"
            );
            assert!(
                sql.contains(
                "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
            ),
                "must close upward over parents so DependencyFailed victims are caught: {sql}"
            );
            assert!(
                sql.contains("db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)"),
                "the thaw must skip the whole poisoned subtree: {sql}"
            );
            assert!(
                sql.contains(&format!(
                    "db.status IN ({})",
                    crate::sql::status::build_in(&BuildStatus::REQUEUEABLE)
                )),
                "still thaws the requeueable states for transient causes: {sql}"
            );
        }
    }
}
