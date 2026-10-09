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
        let walk = crate::graph::walks::begin_walk(db).await?;
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
    };
    let walk = crate::graph::walks::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(query.bind([Value::Uuid(Some(scope.evaluation().into_inner()))]))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

fn requeue_closure_update(blocked: &str) -> String {
    format!(
        r#"
        UPDATE derivation_build db
        SET status = {created}, attempt = 0,
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.derivation IN (SELECT derivation FROM closure)
          AND db.status IN ({requeueable}){blocked}
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        created = crate::sql::status::build(BuildStatus::Created),
        requeueable = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE),
    )
}

fn aborted_blocked_cte_body() -> String {
    bounded_dependency_closure_cte_body(
        "aborted_blocked",
        &aborted_in_evaluation("$1"),
        ClosureDirection::WantedBy,
        &non_passthrough_predicate("e.derivation"),
        Some("closure"),
    )
}

const SKIP_ABORTED: &str =
    "\n          AND db.derivation NOT IN (SELECT derivation FROM aborted_blocked)";

fn requeue_failed_closure_fresh_sql() -> String {
    format!(
        "{},\n    {}\n{}",
        eval_closure_cte(),
        aborted_blocked_cte_body(),
        requeue_closure_update(SKIP_ABORTED),
    )
}

pub(super) fn requeue_failed_closure_blocked_sql() -> String {
    format!(
        "{},\n    {}\n{}",
        requeue_ctes("SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1"),
        aborted_blocked_cte_body(),
        requeue_closure_update(&format!(
            "\n          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked){SKIP_ABORTED}"
        )),
    )
}

/// Bypass the deterministic-failure block: the user asked for the rebuild.
fn retry_build_closure_sql() -> String {
    let dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed);
    let dependents = bounded_dependency_closure_cte_body(
        "dependents",
        "SELECT $2::uuid",
        ClosureDirection::WantedBy,
        &format!(
            "{} AND EXISTS (SELECT 1 FROM derivation_build p \
             WHERE p.derivation = e.derivation AND p.status = {dependency_failed})",
            non_passthrough_predicate("e.derivation"),
        ),
        Some("closure"),
    );
    format!(
        r#"
        WITH RECURSIVE {closure},
        {dependents}
        UPDATE derivation_build db
        SET status = {created}, attempt = 0,
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.derivation IN (SELECT derivation FROM dependents)
          AND CASE WHEN db.derivation = $2 THEN db.status IN ({retryable})
                   ELSE db.status = {dependency_failed} END
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        closure = eval_closure_cte_body(),
        created = crate::sql::status::build(BuildStatus::Created),
        retryable = crate::sql::status::build_in(&BuildStatus::RETRYABLE),
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
    let walk = crate::graph::walks::begin_walk(db).await?;
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
            "\n          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)"
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
        let walk = crate::graph::walks::begin_walk(db).await?;
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
        flags = [Walk];

    REQUEUE_FAILED_CLOSURE_BLOCKED = requeue_failed_closure_blocked_sql,
        params = [EvaluationId],
        tier = Walk,
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
                "db.status IN ({}) AND db.derivation NOT IN (SELECT derivation FROM aborted_blocked) RETURNING",
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
    fn no_heal_of_an_evaluation_thaws_what_its_user_aborted() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let aborted = norm(aborted_in_evaluation("$1"));
        for sql in [
            norm(requeue_failed_closure_fresh_sql()),
            norm(requeue_failed_closure_blocked_sql()),
        ] {
            assert!(
                sql.contains(&format!("aborted_blocked(derivation) AS ({aborted} UNION")),
                "{sql}"
            );
            assert!(
                sql.contains(
                    "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
                ),
                "the dependents of the aborted build stay failed: {sql}"
            );
            assert!(
                sql.contains("db.derivation NOT IN (SELECT derivation FROM aborted_blocked)"),
                "{sql}"
            );
        }
    }

    #[test]
    fn a_retry_thaws_the_build_and_only_the_dependents_it_failed() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(retry_build_closure_sql());
        let dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed);
        assert!(
            sql.starts_with("WITH RECURSIVE closure(derivation) AS"),
            "the walk stays inside the evaluation: {sql}"
        );
        assert!(
            sql.contains("dependents(derivation) AS (SELECT $2::uuid UNION"),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "WHERE p.derivation = e.derivation AND p.status = {dependency_failed}"
            )),
            "the walk stops at a parent that failed on its own: {sql}"
        );
        assert!(
            sql.contains(&format!(
                "CASE WHEN db.derivation = $2 THEN db.status IN ({}) ELSE db.status = {dependency_failed} END",
                crate::sql::status::build_in(&BuildStatus::RETRYABLE)
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
