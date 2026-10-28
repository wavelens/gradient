/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::{
    predicates::{aborted_in_evaluation, failure_spreads_predicate},
    walks::{ClosureDirection, bounded_dependency_closure_cte_body, eval_closure_cte_body},
};
use crate::sql::StatusWrite;
use crate::status::TransitionChange;

use super::transitions::returned_transitions;
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};

fn cascade_dependency_failed_sql() -> String {
    let cte = format!(
        "WITH RECURSIVE {}",
        bounded_dependency_closure_cte_body(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
            &failure_spreads_predicate("e"),
            None,
        )
    );
    format!(
        r#"
    {cte}
    UPDATE derivation_build AS db
    SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.status IN ({cascade_target}) AND {not_in_flight}
      AND db.derivation IN (SELECT derivation FROM wanted_by WHERE derivation <> $1)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        dependency_failed = crate::sql::status::to(BuildStatus::CASCADE_DEPENDENCY_FAILED),
        cascade_target = crate::sql::status::from(BuildStatus::CASCADE_DEPENDENCY_FAILED),
        not_in_flight = not_in_flight(),
    )
}

fn not_in_flight() -> String {
    crate::scheduling::assignment_record::no_open_assignment_predicate(
        &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
    )
}

crate::sql_fn! {
    CASCADE_DEPENDENCY_FAILED = cascade_dependency_failed_sql,
        params = [DerivationId],
        tier = Walk,
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::CASCADE_DEPENDENCY_FAILED);
}

pub async fn cascade_dependency_failed<C>(
    db: &C,
    failed_derivation: DerivationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    // The unbounded upward walk is the widest frontier in the system.
    // It is getting the raised `work_mem`.
    let walk = crate::graph::walks::begin_walk(db, &CASCADE_DEPENDENCY_FAILED).await?;
    let rows = walk
        .query_all_raw(
            CASCADE_DEPENDENCY_FAILED.bind([Value::Uuid(Some(failed_derivation.into_inner()))]),
        )
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

/// The reactive cascade is firing only on a fresh failure transition.
/// A thaw or a concurrent re-fail can leave a parent pending above a failed dependency.
/// This sweep is catching that parent.
pub async fn repair_dependency_failed<C>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &DEPENDENCY_FAILED_REPAIR).await?;
    let rows = walk
        .query_all_raw(DEPENDENCY_FAILED_REPAIR.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

fn dependency_failed_repair_sql() -> String {
    let dependency_failed = crate::sql::status::to(BuildStatus::CASCADE_DEPENDENCY_FAILED);
    let cascade_target = crate::sql::status::from(BuildStatus::CASCADE_DEPENDENCY_FAILED);
    let terminal_failure = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE);
    let prelude = format!(
        "WITH RECURSIVE {closure},\n    {wanted_by}",
        closure = eval_closure_cte_body(),
        wanted_by = bounded_dependency_closure_cte_body(
            "wanted_by",
            &format!(
                "SELECT derivation FROM (SELECT derivation FROM derivation_build \
                 WHERE status IN ({terminal_failure}) \
                   AND derivation IN (SELECT derivation FROM closure) \
                 UNION {aborted}) seed",
                aborted = aborted_in_evaluation("$1"),
            ),
            ClosureDirection::WantedBy,
            &failure_spreads_predicate("e"),
            Some("closure"),
        ),
    );

    format!(
        r#"
    {prelude}
    UPDATE derivation_build AS db
    SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.status IN ({cascade_target}) AND {not_in_flight}
      AND db.derivation IN (SELECT derivation FROM wanted_by)
      AND db.derivation IN (SELECT derivation FROM closure)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        not_in_flight = not_in_flight(),
    )
}

crate::sql_fn! {
    DEPENDENCY_FAILED_REPAIR = dependency_failed_repair_sql,
        params = [EvaluationId],
        tier = Walk,
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::CASCADE_DEPENDENCY_FAILED);
}

#[cfg(test)]
mod tests {
    use super::super::requeue::{
        requeue_failed_closure_blocked_sql, requeue_failed_shared_builds_sql,
    };
    use super::*;

    #[test]
    fn a_failure_walk_enters_a_passthrough_over_a_runtime_dependency_only() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let fence = norm(failure_spreads_predicate("e"));
        assert!(
            fence.starts_with("((e.kind IN (1, 2) OR NOT EXISTS")
                && fence.contains("rb.cache_available"),
            "a build input of a passthrough is no failure of the passthrough: {fence}"
        );
        for sql in [
            norm(cascade_dependency_failed_sql()),
            norm(dependency_failed_repair_sql()),
            norm(requeue_failed_shared_builds_sql()),
            norm(requeue_failed_closure_blocked_sql()),
        ] {
            assert!(
                sql.contains(&fence),
                "the upward walk enters a passthrough over a build input: {sql}"
            );
        }
    }

    #[test]
    fn dependency_failed_repair_sql_mirrors_the_cascade() {
        let sql = dependency_failed_repair_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let terminal_failure = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE);
        let cascade_target = crate::sql::status::from(BuildStatus::CASCADE_DEPENDENCY_FAILED);
        assert!(
            !BuildStatus::TERMINAL_FAILURE.contains(&BuildStatus::Aborted)
                && !BuildStatus::CASCADE_DEPENDENCY_FAILED
                    .from
                    .contains(&BuildStatus::Building),
            "seed excludes Aborted (retried, not permanent); target excludes Building"
        );
        assert!(
            sql.contains(&format!(
                "FROM derivation_build WHERE status IN ({terminal_failure})"
            )),
            "must seed from the terminal-failed set: {sql}"
        );
        assert!(
            sql.contains(&format!(
                "SET status = {}",
                crate::sql::status::build(BuildStatus::DependencyFailed)
            )),
            "must fail parents to DependencyFailed: {sql}"
        );
        assert!(
            sql.contains(&format!("db.status IN ({cascade_target})")),
            "must only touch non-terminal shared builds (never terminal-success): {sql}"
        );
        assert!(
            sql.contains("old.status AS from_status"),
            "must capture the pre-update status for the effects emitter: {sql}"
        );
        let in_flight_guard = format!("AND {}", not_in_flight());
        assert!(
            sql.contains(&in_flight_guard)
                && cascade_dependency_failed_sql().contains(&in_flight_guard),
            "neither write may take a shared build away from its open dispatch: {sql}"
        );
        assert!(
            sql.contains(
                "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
            ),
            "must walk parents upward via the dependency edge: {sql}"
        );
        assert!(
            sql.contains("RETURNING db.derivation"),
            "must return failed derivations so the caller can finalize their evals: {sql}"
        );
    }

    #[test]
    fn a_build_the_evaluation_aborted_fails_its_dependents_like_a_failure() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(dependency_failed_repair_sql());
        assert!(
            sql.contains(&format!(
                "UNION {}) seed",
                norm(aborted_in_evaluation("$1"))
            )),
            "{sql}"
        );
    }

    #[test]
    fn dependency_failed_repair_sql_bounds_to_eval_closure() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let scoped = norm(dependency_failed_repair_sql());
        assert!(
            scoped.contains("WITH RECURSIVE closure(derivation) AS"),
            "the sweep must walk the eval closure: {scoped}"
        );
        assert!(
            scoped.contains(&format!(
                "WHERE status IN ({terminal_failure}) AND derivation IN (SELECT derivation FROM closure)",
                terminal_failure = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE),
            )),
            "the seed must be the closure's terminal-failed shared builds: {scoped}"
        );
        assert!(
            scoped.contains("WHERE EXISTS (SELECT 1 FROM closure x WHERE x.derivation = t.next)"),
            "the walk must stay inside the closure: {scoped}"
        );
        assert!(
            scoped
                .matches("IN (SELECT derivation FROM closure)")
                .count()
                >= 2,
            "seed and UPDATE must be bounded to the closure: {scoped}"
        );
    }
}
