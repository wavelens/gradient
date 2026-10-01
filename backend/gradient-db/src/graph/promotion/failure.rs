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

/// Recursively mark every parent of `failed_derivation` `DependencyFailed`.
/// Walks the global `derivation_dependency` graph upward: any non-terminal
/// shared build (`Created`/`Queued`/`FailedTransient`) reachable from the failure can
/// never build, so it is failed in one recursive statement. Returns the changes
/// it made so the caller can feed [`crate::status::emit_transition_effects`].
pub async fn cascade_dependency_failed<C>(
    db: &C,
    failed_derivation: DerivationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    // The unbounded upward walk is the widest frontier in the system (940k rows
    // for 68k distinct nodes), so it gets the raised `work_mem`.
    let walk = crate::graph::walks::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(
            CASCADE_DEPENDENCY_FAILED.bind([Value::Uuid(Some(failed_derivation.into_inner()))]),
        )
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

/// Proactive mirror of [`cascade_dependency_failed`], bounded to one evaluation's
/// dependency closure. The reactive cascade fires only on a fresh terminal-failure
/// *transition*, so it cannot reach a shared build that becomes non-terminal **after**
/// its dependency already failed: [`requeue_failed_shared_builds`](super::requeue_failed_shared_builds) /
/// [`requeue_failed_closure`](super::requeue_failed_closure) thaw a parent back to `Created` without
/// re-checking its (still-failed) dependency, and a concurrent eval can re-fail a
/// dependency after the parent was thawed. Such a parent can never build, yet
/// sits `Created`/`Queued`/`FailedTransient` forever - its dependency's failure keeps
/// `blocking_deps` above zero, so it is never promoted (or is un-promoted again if it
/// was), and `check_evaluation_done` never finalizes its evaluation. This walks
/// `derivation_dependency` upward from every terminal-failed shared build in the closure and fails each reachable non-terminal shared build in one
/// statement (the recursive term traverses the graph structurally, so a whole
/// poisoned subtree converges per pass). Returns the changes it made so the caller
/// can fan out the effects and finalize the now-settled evaluations.
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

/// Recursive upward walk from every terminal-failed shared build in the evaluation's
/// closure that fails each reachable non-terminal shared build. Mirrors the reactive
/// [`cascade_dependency_failed`] terminal-failed set (it excludes `Aborted`, which
/// is retried, not permanent). The failed roots are excluded from the UPDATE by the
/// cascade-target predicate, so the sweep is idempotent. The seed, the walk and the
/// UPDATE are all bounded to the eval closure ($1): this executes right after the
/// requeue thaw, on an event, and re-fails that eval's own thawed victims without
/// ever scanning the whole table.
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

    /// A failure must not cross a passthrough. A shared build available in a cache takes finished
    /// bytes off an upstream, so an input that can never build neither dooms it
    /// nor reaches anything above it; measured in the e2e VM's phase 10g, where
    /// busybox's unbuildable source FODs cascaded `DependencyFailed` onto the
    /// passed through shared build itself (#666). Every upward walk that carries a failure
    /// fences on the same predicate, so the cascade, its sweep and the thaw's
    /// blocked set can never disagree about who a failure reaches.
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

    /// The proactive dependency-failed sweep must mirror the reactive cascade:
    /// seed the recursive walk from the terminal-failed set the cascade reacts
    /// to (NOT `Aborted`), fail only non-terminal shared builds to `DependencyFailed`,
    /// and walk parents upward via the dependency edge. Getting the seed or
    /// target set wrong either misses the dead zone or clobbers terminal-success
    /// shared builds, so pin the SQL shape (no live DB in unit tests).
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

    /// The sweep is bounded to one eval's dependency closure ($1) and has no
    /// full-table form left: the seed, the walk and the UPDATE all carry the
    /// membership filter, so an event-driven heal re-fails its own thawed victims
    /// without ever scanning the table.
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
