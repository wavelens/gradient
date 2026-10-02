/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::{
    predicates::non_passthrough_predicate,
    walks::{ClosureDirection, bounded_dependency_closure_cte_body, eval_closure_cte_body},
};
use crate::status::TransitionChange;

use super::transitions::returned_transitions;
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};

const CASCADE_TARGET: [BuildStatus; 3] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::FailedTransient,
];

fn cascade_dependency_failed_sql() -> String {
    let cte = format!(
        "WITH RECURSIVE {}",
        bounded_dependency_closure_cte_body(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
            &non_passthrough_predicate("e.derivation"),
            None,
        )
    );
    format!(
        r#"
    {cte}
    UPDATE derivation_build AS db
    SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.status IN ({cascade_target})
      AND db.derivation IN (SELECT derivation FROM wanted_by WHERE derivation <> $1)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed),
        cascade_target = crate::sql::status::build_in(&CASCADE_TARGET),
    )
}

crate::sql_fn! {
    CASCADE_DEPENDENCY_FAILED = cascade_dependency_failed_sql,
        params = [DerivationId],
        tier = Walk,
        flags = [Walk];
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
    let walk = crate::graph::walks::begin_walk(db).await?;
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
    let walk = crate::graph::walks::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(DEPENDENCY_FAILED_REPAIR.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

fn dependency_failed_repair_sql() -> String {
    let dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed);
    let cascade_target = crate::sql::status::build_in(&CASCADE_TARGET);
    let terminal_failure = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE);
    let prelude = format!(
        "WITH RECURSIVE {closure},\n    {wanted_by}",
        closure = eval_closure_cte_body(),
        wanted_by = bounded_dependency_closure_cte_body(
            "wanted_by",
            &format!(
                "SELECT derivation FROM derivation_build \
                 WHERE status IN ({terminal_failure}) \
                   AND derivation IN (SELECT derivation FROM closure)"
            ),
            ClosureDirection::WantedBy,
            &non_passthrough_predicate("e.derivation"),
            Some("closure"),
        ),
    );

    format!(
        r#"
    {prelude}
    UPDATE derivation_build AS db
    SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.status IN ({cascade_target})
      AND db.derivation IN (SELECT derivation FROM wanted_by)
      AND db.derivation IN (SELECT derivation FROM closure)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#
    )
}

crate::sql_fn! {
    DEPENDENCY_FAILED_REPAIR = dependency_failed_repair_sql,
        params = [EvaluationId],
        tier = Walk,
        flags = [Walk];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::promotion::requeue::{
        requeue_failed_closure_blocked_sql, requeue_failed_shared_builds_sql,
    };

    #[test]
    fn a_failure_walk_never_enters_a_passthrough() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let fence = norm(non_passthrough_predicate("e.derivation"));
        assert!(
            fence.contains("rb.cache_available"),
            "the fence must read the passthrough flag: {fence}"
        );
        for sql in [
            norm(cascade_dependency_failed_sql()),
            norm(dependency_failed_repair_sql()),
            norm(requeue_failed_shared_builds_sql()),
            norm(requeue_failed_closure_blocked_sql()),
        ] {
            assert!(
                sql.contains(&fence),
                "upward walk crosses a passthrough: {sql}"
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
        let cascade_target = crate::sql::status::build_in(&CASCADE_TARGET);
        assert!(
            !BuildStatus::TERMINAL_FAILURE.contains(&BuildStatus::Aborted)
                && !CASCADE_TARGET.contains(&BuildStatus::Building),
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
