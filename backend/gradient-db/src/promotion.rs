/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Anchor transitions that are not the readiness promotion: the substitution of
//! anchors an evaluation found whole in our cache, the failure cascade and its
//! eval-scoped sweep, the requeue thaws, and the dispatch gate. Promotion itself
//! lives in [`crate::readiness`], which owns the `fetchable` / `unready_deps`
//! counters every gate is built from.
//!
//! The dispatch gate reads the queue invariant instead of re-deriving readiness:
//! `Queued` means [`crate::graph_sql::gates_predicate`] held when the anchor was
//! promoted, and the event that breaks one of those gates un-promotes the row.
//! Re-evaluating the gates per dispatch candidate is the per-row work #591 removed.

use crate::graph_sql::{
    ClosureDirection, bounded_dependency_closure_cte_body, dependency_closure_cte,
    dependency_closure_cte_body, eval_closure_cte, eval_closure_cte_body,
};
use crate::status::TransitionChange;
use crate::status_sql;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_types::DerivationId;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseTransaction, DbErr, QueryResult, Statement,
    TransactionTrait, Value,
};

const CASCADE_TARGET: [BuildStatus; 3] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::FailedTransient,
];

/// Collect the `derivation` column of a `RETURNING derivation` result set. The
/// bulk transitions return the anchors they actually moved so the caller can fan
/// the CI status reactor out over exactly those (and only those) builds.
pub(crate) fn returned_derivations(rows: Vec<QueryResult>) -> Vec<DerivationId> {
    rows.into_iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

/// Collect `RETURNING db.derivation, old.status AS from_status, db.status AS
/// to_status` rows into the typed changes the effects emitter consumes. Bulk
/// statements capture the pre-update status via a `FROM derivation_build old`
/// self-join on the primary key (Postgres evaluates `old` against the snapshot).
pub(crate) fn returned_transitions(rows: Vec<QueryResult>) -> Vec<TransitionChange> {
    rows.into_iter()
        .filter_map(|r| {
            let derivation = r.try_get::<uuid::Uuid>("", "derivation").ok()?;
            let from = BuildStatus::try_from(r.try_get::<i32>("", "from_status").ok()?).ok()?;
            let to = BuildStatus::try_from(r.try_get::<i32>("", "to_status").ok()?).ok()?;
            Some(TransitionChange {
                derivation: DerivationId::new(derivation),
                from,
                to,
            })
        })
        .collect()
}

/// Changes for rows a statement moved from a statically-known status (e.g. a
/// `WHERE status = Created` promote): no self-join needed, the predicate is the proof.
pub(crate) fn transitions_from(
    derivations: Vec<DerivationId>,
    from: BuildStatus,
    to: BuildStatus,
) -> Vec<TransitionChange> {
    derivations
        .into_iter()
        .map(|derivation| TransitionChange {
            derivation,
            from,
            to,
        })
        .collect()
}

/// Anchors an evaluation found whole in our cache move from `Created` to
/// `Substituted`; a new anchor is inserted that way, this catches the ones a
/// prior evaluation left pending. Returns the transitions for the effects
/// emitter.
pub async fn substitute_created_anchors<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            substitute_created_anchors_sql(),
            [ids.into()],
        ))
        .await?;

    Ok(returned_transitions(rows))
}

fn substitute_created_anchors_sql() -> String {
    format!(
        r#"
        UPDATE derivation_build AS db
        SET status = {substituted}, substituted = true,
            updated_at = (now() AT TIME ZONE 'UTC')
        FROM derivation_build old
        WHERE old.id = db.id AND db.status = {created} AND db.derivation = ANY($1::uuid[])
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        substituted = status_sql::build(BuildStatus::Substituted),
        created = status_sql::build(BuildStatus::Created),
    )
}

/// Recursively mark every dependent of `failed_derivation` `DependencyFailed`.
/// Walks the global `derivation_dependency` graph upward: any non-terminal
/// anchor (`Created`/`Queued`/`FailedTransient`) reachable from the failure can
/// never build, so it is failed in one recursive statement. Returns the changes
/// it made so the caller can feed [`crate::status::emit_transition_effects`].
pub async fn cascade_dependency_failed<C>(
    db: &C,
    failed_derivation: DerivationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let cte = dependency_closure_cte(
        "dependents",
        "SELECT $1::uuid",
        ClosureDirection::Dependents,
    );
    // The unbounded upward walk is the widest frontier in the system (940k rows
    // for 68k distinct nodes), so it gets the raised `work_mem`.
    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                r#"
            {cte}
            UPDATE derivation_build AS db
            SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
            FROM derivation_build old
            WHERE old.id = db.id
              AND db.status IN ({cascade_target})
              AND db.derivation IN (SELECT derivation FROM dependents WHERE derivation <> $1)
            RETURNING db.derivation, old.status AS from_status, db.status AS to_status
            "#,
                dependency_failed = status_sql::build(BuildStatus::DependencyFailed),
                cascade_target = status_sql::build_in(&CASCADE_TARGET),
            ),
            [Value::Uuid(Some(failed_derivation.into_inner()))],
        ))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

/// Proactive mirror of [`cascade_dependency_failed`], bounded to one evaluation's
/// dependency closure. The reactive cascade fires only on a fresh terminal-failure
/// *transition*, so it cannot reach an anchor that becomes non-terminal **after**
/// its dependency already failed: `requeue_failed_anchors` /
/// `requeue_failed_closure_for_eval` thaw a dependent back to `Created` without
/// re-checking its (still-failed) dependency, and a concurrent eval can re-fail a
/// dependency after the dependent was thawed. Such a dependent can never build, yet
/// sits `Created`/`Queued`/`FailedTransient` forever - its dependency's failure keeps
/// `unready_deps` above zero, so it is never promoted (or is un-promoted again if it
/// was), and `check_evaluation_done` never finalizes its evaluation. This walks `derivation_dependency` upward from every terminal-failed
/// anchor in the closure and fails each reachable non-terminal anchor in one
/// statement (the recursive term traverses the graph structurally, so a whole
/// poisoned subtree converges per pass). Returns the changes it made so the caller
/// can fan out the effects and finalize the now-settled evaluations.
pub async fn reconcile_dependency_failed<C>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            dependency_failed_reconcile_sql(),
            [Value::Uuid(Some(evaluation.into_inner()))],
        ))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

/// Recursive upward walk from every terminal-failed anchor in the evaluation's
/// closure that fails each reachable non-terminal anchor. Mirrors the reactive
/// [`cascade_dependency_failed`] terminal-failed set (it excludes `Aborted`, which
/// is retried, not permanent). The failed roots are excluded from the UPDATE by the
/// cascade-target predicate, so the sweep is idempotent. The seed, the walk and the
/// UPDATE are all bounded to the eval closure ($1): this runs right after the
/// requeue thaw, on an event, and re-fails that eval's own thawed victims without
/// ever scanning the whole table.
fn dependency_failed_reconcile_sql() -> String {
    let dependency_failed = status_sql::build(BuildStatus::DependencyFailed);
    let cascade_target = status_sql::build_in(&CASCADE_TARGET);
    let terminal_failure = status_sql::build_in(&BuildStatus::TERMINAL_FAILURE);
    let prelude = format!(
        "WITH RECURSIVE {closure},\n    {dependents}",
        closure = eval_closure_cte_body(),
        dependents = bounded_dependency_closure_cte_body(
            "dependents",
            &format!(
                "SELECT derivation FROM derivation_build \
                 WHERE status IN ({terminal_failure}) \
                   AND derivation IN (SELECT derivation FROM closure)"
            ),
            ClosureDirection::Dependents,
            "e.derivation IN (SELECT derivation FROM closure)",
        ),
    );

    format!(
        r#"
    {prelude}
    UPDATE derivation_build AS db
    SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
    FROM derivation_build old
    WHERE old.id = db.id
      AND db.status IN ({cascade_target})
      AND db.derivation IN (SELECT derivation FROM dependents)
      AND db.derivation IN (SELECT derivation FROM closure)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#
    )
}

/// The dispatch gate reads the invariant: `Queued` means the gates held when the
/// anchor was promoted, and a regression un-promotes. Reachability still filters
/// anchors left queued after their last referencing evaluation was torn down.
///
/// The open-`dispatched_job` arm is a dispatch gate, not a readiness one, so it
/// lives here and not in `readiness::promote`: an anchor that is already out is
/// still perfectly promotable and must stay `Queued` for the report that closes
/// it. The tracker's in-memory `untracked` filter is empty after a core respawn;
/// the row is what survives, so the select carries the gate itself.
pub async fn find_ready_anchors<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<gradient_types::MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;
    gradient_types::EDerivationBuild::find()
        .from_raw_sql(Statement::from_string(
            DatabaseBackend::Postgres,
            find_ready_anchors_sql(),
        ))
        .all(db)
        .await
}

fn find_ready_anchors_sql() -> String {
    let not_in_flight = crate::dispatch_record::no_open_dispatch_predicate(
        &crate::dispatch_record::build_job_key_sql("db.id"),
    );

    format!(
        r#"
        SELECT db.*
        FROM derivation_build db
        WHERE db.status = {queued}
          AND {not_in_flight}
          AND EXISTS (
            SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation)
        ORDER BY
            (SELECT count(*)
               FROM derivation_dependency dd
              WHERE dd.derivation = db.derivation) DESC,
            db.updated_at ASC
        "#,
        queued = status_sql::build(BuildStatus::Queued),
    )
}

/// SQL predicate: the `derivation_build` aliased `alias` has a recorded
/// deterministic build failure - the builder ran and exited non-zero. Rebuilding
/// the identical derivation reproduces it, so a fresh evaluation must not thaw it
/// (else it loops: re-queue -> rebuild -> same non-zero exit -> re-queue). Only a
/// changed drv (a new anchor) or a newly-substitutable output can recover it.
fn deterministic_build_failure(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM build_attempt ba WHERE ba.derivation_build = {alias}.id \
         AND ba.outcome = {outcome} AND ba.reason = {reason})",
        outcome = status_sql::attempt_outcome(AttemptOutcome::Failed),
        reason = status_sql::attempt_reason(AttemptFailureReason::BuilderNonzero),
    )
}

/// Re-queue anchors a previous evaluation left in a terminal-failure state
/// (`FailedPermanent`/`Aborted`/`DependencyFailed`/`FailedTimeout`) back to
/// `Created`, for the derivations a new evaluation needs. A new evaluation is a
/// fresh build intent - the upstream cache, network, or a transient cause may
/// have changed since the global anchor failed - so it retries rather than
/// inheriting the stale failure. Anchors with a [`deterministic_build_failure`]
/// are excluded: their non-zero builder exit is reproducible, so re-queueing only
/// loops the fleet. Build-once success states (`Completed`/`Substituted`) are
/// never touched. Returns the thaws it made, so the caller can feed
/// [`crate::status::emit_transition_effects`].
pub async fn requeue_failed_anchors<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let sql = requeue_failed_anchors_sql();
    let mut changes = Vec::new();
    for chunk in derivations.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        let rows = db
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                [ids.into()],
            ))
            .await?;
        changes.extend(returned_transitions(rows));
    }

    Ok(changes)
}

/// `WITH RECURSIVE` prelude binding a requeue candidate `closure` (the downward
/// build closure of the candidates) and the `deterministic_blocked` subset a
/// reproducible build failure permanently poisons. `deterministic_blocked` seeds
/// from every anchor in the closure with a [`deterministic_build_failure`] and
/// closes upward over `derivation_dependency` (bounded to the closure), so a
/// `DependencyFailed` dependent of such a failure is caught even though it never
/// ran a build of its own. A thaw must exclude this whole set: its members can
/// never build, and thawing one back to `Created` only re-enters the demote<->
/// thaw oscillation with [`reconcile_dependency_failed`] that hangs the eval in
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
            ClosureDirection::Dependents,
            "e.derivation IN (SELECT derivation FROM closure)",
        ),
    )
}

fn requeue_failed_anchors_sql() -> String {
    let ctes = requeue_ctes("SELECT unnest($1::uuid[])");
    format!(
        r#"
        {ctes}
        UPDATE derivation_build db
        SET status = {created}, attempt = 0,
            updated_at = (now() AT TIME ZONE 'UTC')
        FROM derivation_build old
        WHERE old.id = db.id
          AND db.derivation = ANY($1) AND db.status IN ({requeueable})
          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        created = status_sql::build(BuildStatus::Created),
        requeueable = status_sql::build_in(&BuildStatus::REQUEUEABLE),
    )
}

/// Re-queue terminal-failed anchors across the full build-dependency **closure**
/// of an evaluation's anchors, not just the derivations its walk re-reported.
/// `requeue_failed_anchors` only thaws the eval's own derivations; a transitive
/// dependency a prior eval left terminal-failed - and which this eval pruned or
/// never re-walked (so it has no `build_job` here) - stays failed forever and
/// blocks its dependents with no dispatch (hence no failure) to trigger any
/// reactive heal. Walks `derivation_dependency` down from the eval's anchors and
/// resets every `FailedPermanent`/`Aborted`/`DependencyFailed`/`FailedTimeout`
/// node to `Created` so promotion (which keys on any `build_job`, not this eval's)
/// can rebuild the failed subtree bottom-up. Anchors with a
/// [`deterministic_build_failure`] are excluded, as in [`requeue_failed_anchors`].
/// Returns the thaws it made, so the caller can feed
/// [`crate::status::emit_transition_effects`].
pub async fn requeue_failed_closure_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            requeue_failed_closure_for_eval_sql(),
            [Value::Uuid(Some(evaluation.into_inner()))],
        ))
        .await?;

    Ok(returned_transitions(rows))
}

fn requeue_failed_closure_for_eval_sql() -> String {
    let ctes = requeue_ctes("SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1");
    format!(
        r#"
        {ctes}
        UPDATE derivation_build db
        SET status = {created}, attempt = 0,
            updated_at = (now() AT TIME ZONE 'UTC')
        FROM derivation_build old
        WHERE old.id = db.id
          AND db.derivation IN (SELECT derivation FROM closure)
          AND db.status IN ({requeueable})
          AND db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        created = status_sql::build(BuildStatus::Created),
        requeueable = status_sql::build_in(&BuildStatus::REQUEUEABLE),
    )
}

/// Reconcile anchor state from cache state across an evaluation's dependency
/// closure: any anchor whose outputs are **all** present in our cache
/// (`cached_path.file_hash`) is marked `Completed`, even if a
/// requeue / dependency-failed cascade / demote previously reset it. The dispatch
/// gate keys on the build-graph anchor state, which repeatedly desyncs from the
/// durable cache state - a derivation whose artifacts exist sits `Created` and
/// blocks its dependents with nothing to build. Cache presence is the ground truth
/// for "is this built", so trust it here; the reactive heals
/// (`demote_referrers_of` / absent-orphan recovery) remain the backstop for the
/// rare case where a cached output's runtime closure is itself incomplete. Returns
/// the changes it made, so the caller can advance the dependents of what it just
/// settled; an anchor already terminal-success is left alone, since it has nothing
/// left for this statement to write.
pub async fn reconcile_cached_anchors_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr> {
    let cte = eval_closure_cte();
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                r#"
            {cte}
            UPDATE derivation_build db
            SET status = CASE WHEN db.status IN ({terminal_success}) THEN db.status ELSE {completed} END,
                updated_at = (now() AT TIME ZONE 'UTC')
            FROM derivation_build old
            WHERE old.id = db.id
              AND db.derivation IN (SELECT derivation FROM closure)
              AND db.status NOT IN ({terminal_success})
              AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation)
              AND NOT EXISTS (
                SELECT 1 FROM derivation_output o
                LEFT JOIN cached_path cp ON cp.hash = o.hash AND cp.file_hash IS NOT NULL
                WHERE o.derivation = db.derivation AND cp.hash IS NULL)
            RETURNING db.derivation, old.status AS from_status, db.status AS to_status
            "#,
                terminal_success = status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS),
                completed = status_sql::build(BuildStatus::Completed),
            ),
            [Value::Uuid(Some(evaluation.into_inner()))],
        ))
        .await?;

    Ok(returned_transitions(rows))
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
                outcome = status_sql::attempt_outcome(AttemptOutcome::Failed),
                reason = status_sql::attempt_reason(AttemptFailureReason::BuilderNonzero),
            ),
        );
        assert_eq!(status_sql::attempt_outcome(AttemptOutcome::Failed), 3);
        assert_eq!(
            status_sql::attempt_reason(AttemptFailureReason::BuilderNonzero),
            5
        );
    }

    /// Both requeue paths must carry the deterministic-failure exclusion, keep
    /// `FailedPermanent` requeueable for transient causes, and never thaw a
    /// build-once success.
    #[test]
    fn requeue_keeps_failed_permanent_but_excludes_deterministic() {
        assert!(BuildStatus::REQUEUEABLE.contains(&BuildStatus::FailedPermanent));
        for s in BuildStatus::TERMINAL_SUCCESS {
            assert!(!BuildStatus::REQUEUEABLE.contains(&s));
        }
    }

    /// A thaw must exclude not just a derivation's own reproducible failure but
    /// the whole subtree a deterministic failure poisons: a `DependencyFailed`
    /// dependent never ran a build of its own, so keying the exclusion on the
    /// anchor's own attempts alone re-thaws it forever (the demote<->thaw
    /// oscillation that hangs the eval). Pin that both requeue SQLs build the
    /// closure + `deterministic_blocked` walk and exclude that set (no live DB).
    #[test]
    fn requeue_excludes_the_deterministic_blocked_subtree() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let deterministic = norm(deterministic_build_failure("dbf"));
        for sql in [
            norm(requeue_failed_anchors_sql()),
            norm(requeue_failed_closure_for_eval_sql()),
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
                "must close upward over dependents so DependencyFailed victims are caught: {sql}"
            );
            assert!(
                sql.contains("db.derivation NOT IN (SELECT derivation FROM deterministic_blocked)"),
                "the thaw must skip the whole poisoned subtree: {sql}"
            );
            assert!(
                sql.contains(&format!(
                    "db.status IN ({})",
                    status_sql::build_in(&BuildStatus::REQUEUEABLE)
                )),
                "still thaws the requeueable states for transient causes: {sql}"
            );
        }
    }

    /// The proactive dependency-failed sweep must mirror the reactive cascade:
    /// seed the recursive walk from the terminal-failed set the cascade reacts
    /// to (NOT `Aborted`), fail only non-terminal anchors to `DependencyFailed`,
    /// and walk dependents upward via the dependency edge. Getting the seed or
    /// target set wrong either misses the dead zone or clobbers terminal-success
    /// anchors, so pin the SQL shape (no live DB in unit tests).
    #[test]
    fn dependency_failed_reconcile_sql_mirrors_the_cascade() {
        let sql = dependency_failed_reconcile_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let terminal_failure = status_sql::build_in(&BuildStatus::TERMINAL_FAILURE);
        let cascade_target = status_sql::build_in(&CASCADE_TARGET);
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
                status_sql::build(BuildStatus::DependencyFailed)
            )),
            "must fail dependents to DependencyFailed: {sql}"
        );
        assert!(
            sql.contains(&format!("db.status IN ({cascade_target})")),
            "must only touch non-terminal anchors (never terminal-success): {sql}"
        );
        assert!(
            sql.contains("FROM derivation_build old") && sql.contains("old.status AS from_status"),
            "must capture the pre-update status for the effects emitter: {sql}"
        );
        assert!(
            sql.contains(
                "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
            ),
            "must walk dependents upward via the dependency edge: {sql}"
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
    fn dependency_failed_reconcile_sql_bounds_to_eval_closure() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let scoped = norm(dependency_failed_reconcile_sql());
        assert!(
            scoped.contains("WITH RECURSIVE closure(derivation) AS"),
            "the sweep must walk the eval closure: {scoped}"
        );
        assert!(
            scoped.contains(&format!(
                "WHERE status IN ({terminal_failure}) AND derivation IN (SELECT derivation FROM closure)",
                terminal_failure = status_sql::build_in(&BuildStatus::TERMINAL_FAILURE),
            )),
            "the seed must be the closure's terminal-failed anchors: {scoped}"
        );
        assert!(
            scoped
                .matches("IN (SELECT derivation FROM closure)")
                .count()
                >= 3,
            "seed, walk, and UPDATE must all be bounded to the closure: {scoped}"
        );
    }

    /// Dispatch trusts the queue invariant: no readiness term is re-evaluated
    /// per row here, only the status and the reachability check.
    #[test]
    fn dispatch_reads_the_queued_invariant_only() {
        let sql = find_ready_anchors_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(sql.contains(&format!(
            "db.status = {}",
            status_sql::build(BuildStatus::Queued)
        )));
        assert!(sql.contains("FROM build_job bj WHERE bj.derivation = db.derivation"));
        assert!(
            !sql.contains("unready_deps")
                && !sql.contains("fetchable")
                && !sql.contains("cached_path"),
            "{sql}"
        );
    }

    /// Cache presence is the ground truth for "built": a pending anchor whose
    /// outputs are whole in our cache is settled `Substituted` without a
    /// dispatch. Only `Created` moves, so a `Queued` anchor already in the
    /// tracker is not pulled out from under the dispatcher.
    #[test]
    fn substitute_created_anchors_moves_only_created_rows() {
        let sql = substitute_created_anchors_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(sql.contains(&format!(
            "SET status = {}",
            status_sql::build(BuildStatus::Substituted)
        )));
        assert!(sql.contains(&format!(
            "db.status = {}",
            status_sql::build(BuildStatus::Created)
        )));
        assert!(sql.contains(
            "RETURNING db.derivation, old.status AS from_status, db.status AS to_status"
        ));
    }

    /// The tracker's `untracked` filter is in memory and empty after a core
    /// respawn; the row is what survives, so the dispatch select carries the
    /// open-row gate itself.
    #[test]
    fn dispatch_refuses_an_anchor_whose_dispatch_row_is_open() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let gate = norm(crate::dispatch_record::no_open_dispatch_predicate(
            &crate::dispatch_record::build_job_key_sql("db.id"),
        ));

        assert!(norm(find_ready_anchors_sql()).contains(&gate));
    }
}
