/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::repair::RepairScope;
use crate::graph::{
    predicates::non_passthrough_predicate,
    walks::{
        ClosureDirection, bounded_dependency_closure_cte_body, dependency_closure_cte_body,
        eval_closure_cte,
    },
};
use crate::status::TransitionChange;

use super::transitions::returned_transitions;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};

/// SQL predicate: the `derivation_build` aliased `alias` has a recorded
/// deterministic build failure - the builder ran and exited non-zero. Rebuilding
/// the identical derivation reproduces it, so a fresh evaluation must not thaw it
/// (else it loops: re-queue -> rebuild -> same non-zero exit -> re-queue). Only a
/// changed drv (a new shared build) or a output newly available in a cache can recover it.
fn deterministic_build_failure(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM build_attempt ba WHERE ba.derivation_build = {alias}.id \
         AND ba.outcome = {outcome} AND ba.reason = {reason})",
        outcome = crate::sql::status::attempt_outcome(AttemptOutcome::Failed),
        reason = crate::sql::status::attempt_reason(AttemptFailureReason::BuilderNonzero),
    )
}

/// Re-queue shared builds a previous evaluation left in a terminal-failure state
/// (`FailedPermanent`/`Aborted`/`DependencyFailed`/`FailedTimeout`) back to
/// `Created`, for the derivations a new evaluation needs. A new evaluation is a
/// fresh build intent - the upstream cache, network, or a transient cause may
/// have changed since the global shared build failed - so it retries rather than
/// inheriting the stale failure. Shared builds with a [`deterministic_build_failure`]
/// are excluded: their non-zero builder exit is reproducible, so re-queueing only
/// loops the fleet. Build-once success states (`Completed`/`Substituted`) are
/// never touched. Returns the thaws it made, so the caller can feed
/// [`crate::status::emit_transition_effects`].
///
/// Like its neighbours here it is a `Walk`: the plan gate proves it with the
/// `work_mem` [`crate::graph::walks::begin_walk`] sets, so it has to run inside
/// one. On a plain connection it plans against the default instead, and a thaw
/// that then exceeds its budget heals nothing and says nothing - the repair pass logs
/// the error and goes on to finalize over shared builds it never re-queued.
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

/// `WITH RECURSIVE` prelude binding a requeue candidate `closure` (the downward
/// build closure of the candidates) and the `deterministic_blocked` subset a
/// reproducible build failure permanently poisons. `deterministic_blocked` seeds
/// from every shared build in the closure with a [`deterministic_build_failure`] and
/// closes upward over `derivation_dependency` (bounded to the closure), so a
/// `DependencyFailed` parent of such a failure is caught even though it never
/// ran a build of its own. A thaw must exclude this whole set: its members can
/// never build, and thawing one back to `Created` only re-enters the demote<->
/// thaw oscillation with [`repair_dependency_failed`] that hangs the eval in
/// `graph_stuck` forever.
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

/// Re-queue terminal-failed shared builds across the full dependency **closure** of an
/// evaluation's names, not just the derivations its walk re-reported: a transitive
/// dependency a prior evaluation left terminal-failed, which this one pruned or
/// never re-walked, would otherwise stay failed forever and block its parents
/// with no dispatch to trigger any reactive heal. Walks `derivation_dependency`
/// down from the names over every edge kind and resets each `REQUEUEABLE` shared build
/// to `Created`; the repair pass then names the thawed closure and promotes it so
/// the failed subtree rebuilds bottom-up.
///
/// A failure is valid for the evaluation that recorded it. A fresh evaluation
/// ([`RepairScope::Eval`]) is a new intent and thaws every failure in its
/// closure, a reproducible builder exit included: one rebuild per evaluation, and
/// same-commit polling is deduplicated before an evaluation exists. An evaluation
/// healing itself ([`RepairScope::Unstick`]) is the same intent again, so there
/// the [`deterministic_build_failure`] subtree stays out, or the unstick would
/// rebuild a permanent failure every sweep. Returns the thaws it made, so the
/// caller can feed [`crate::status::emit_transition_effects`].
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

fn requeue_failed_closure_fresh_sql() -> String {
    format!("{}\n{}", eval_closure_cte(), requeue_closure_update(""))
}

pub(super) fn requeue_failed_closure_blocked_sql() -> String {
    format!(
        "{}\n{}",
        requeue_ctes("SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1"),
        requeue_closure_update(
            "\n          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)"
        ),
    )
}

crate::sql_fn! {
    REQUEUE_FAILED_CLOSURE_FRESH = requeue_failed_closure_fresh_sql,
        params = [EvaluationId],
        tier = Walk,
        flags = [Walk];

    REQUEUE_FAILED_CLOSURE_BLOCKED = requeue_failed_closure_blocked_sql,
        params = [EvaluationId],
        tier = Walk,
        flags = [Walk];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The requeue thaw must skip a reproducible non-zero builder exit, or a
    /// polling-triggered eval loops it forever (re-queue -> rebuild -> same exit).
    /// No live DB in unit tests, so pin the predicate SQL shape and its integers.
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

    /// A fresh evaluation is a new intent: its thaw takes every requeueable shared build
    /// in its closure, a reproducible builder exit included, because a failure is
    /// valid for the evaluation that recorded it and nothing else. Blocked, a
    /// restarted evaluation re-failed on the spot without a single build.
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
                "db.status IN ({}) RETURNING",
                crate::sql::status::build_in(&BuildStatus::REQUEUEABLE)
            )),
            "{sql}"
        );
        assert!(
            !sql.contains("deterministic_blocked") && !sql.contains("build_attempt"),
            "a reproducible failure is retried once per evaluation: {sql}"
        );
    }

    /// The other two thaws are the same intent again, and must exclude not just a
    /// derivation's own reproducible failure but the whole subtree a deterministic
    /// failure poisons: a `DependencyFailed` parent never ran a build of its own,
    /// so keying the exclusion on the shared build's own attempts alone re-thaws it
    /// forever (the demote<->thaw oscillation that hangs the eval). Pin that both
    /// build the closure + `deterministic_blocked` walk and exclude that set.
    #[test]
    fn requeue_excludes_the_deterministic_blocked_subtree() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let deterministic = norm(deterministic_build_failure("dbf"));
        for sql in [
            norm(requeue_failed_shared_builds_sql()),
            norm(requeue_failed_closure_blocked_sql()),
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
