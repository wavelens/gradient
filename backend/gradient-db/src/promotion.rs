/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared build transitions that are not the can-start state promotion: the substitution of
//! shared builds an evaluation found complete in our cache, the failure cascade and its
//! eval-scoped sweep, the requeue thaws, and the dispatch gate. Promotion itself
//! lives in [`crate::can_start`], which owns the `fetchable` / `blocking_deps`
//! counters every gate is built from.
//!
//! The dispatch gate reads the queue invariant instead of re-deriving can-start state:
//! `Queued` means [`crate::graph_sql::gates_predicate`] held when the shared build was
//! promoted, and the event that breaks one of those gates un-promotes the row.
//! Re-evaluating the gates per dispatch candidate is the per-row work #591 removed.

use crate::graph_sql::{
    ClosureDirection, bounded_dependency_closure_cte_body, dependency_closure_cte_body,
    eval_closure_cte, eval_closure_cte_body, non_passthrough_predicate,
};
use crate::repair::RepairScope;
use crate::status::TransitionChange;
use crate::status_sql;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait, Value};

const CASCADE_TARGET: [BuildStatus; 3] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::FailedTransient,
];

/// Collect the `derivation` column of a `RETURNING derivation` result set. The
/// bulk transitions return the shared builds they actually moved so the caller can fan
/// the CI status reactor out over exactly those (and only those) builds.
pub(crate) fn returned_derivations(rows: Vec<QueryResult>) -> Vec<DerivationId> {
    rows.into_iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

/// Collect `RETURNING db.derivation, old.status AS from_status, db.status AS
/// to_status` rows into the typed changes the effects emitter consumes. `old` is
/// Postgres 18's pre-update row, which a self-join used to fetch at the price of
/// a sequential scan once a statement moved many shared builds.
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

/// Shared builds an evaluation found complete in our cache move from `Created` to
/// `Substituted`; a new shared build is inserted that way, this catches the ones a
/// prior evaluation left pending. Returns the transitions for the effects
/// emitter.
pub async fn substitute_created_shared_builds<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    let rows = db
        .query_all_raw(SUBSTITUTE_CREATED_SHARED_BUILDS.bind([ids.into()]))
        .await?;

    Ok(returned_transitions(rows))
}

fn substitute_created_shared_builds_sql() -> String {
    format!(
        r#"
        UPDATE derivation_build AS db
        SET status = {substituted}, substituted = true,
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.status = {created} AND db.derivation = ANY($1::uuid[])
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        substituted = status_sql::build(BuildStatus::Substituted),
        created = status_sql::build(BuildStatus::Created),
    )
}

crate::sql_fn! {
    SUBSTITUTE_CREATED_SHARED_BUILDS = substitute_created_shared_builds_sql,
        params = [DerivationIds(64)];
}

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
        dependency_failed = status_sql::build(BuildStatus::DependencyFailed),
        cascade_target = status_sql::build_in(&CASCADE_TARGET),
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
    let walk = crate::graph_sql::begin_walk(db).await?;
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
/// its dependency already failed: [`requeue_failed_shared_builds`] /
/// [`requeue_failed_closure`] thaw a parent back to `Created` without
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
    let walk = crate::graph_sql::begin_walk(db).await?;
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
    let dependency_failed = status_sql::build(BuildStatus::DependencyFailed);
    let cascade_target = status_sql::build_in(&CASCADE_TARGET);
    let terminal_failure = status_sql::build_in(&BuildStatus::TERMINAL_FAILURE);
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

/// The dispatch gate reads the invariant: `Queued` means the gates held when the
/// shared build was promoted, and a regression un-promotes. Reachability still filters
/// shared builds left queued after their last referencing evaluation was torn down.
///
/// The open-`dispatched_job` arm is a dispatch gate, not a can-start state one, so it
/// lives here and not in `can_start::promote`: a shared build that is already out is
/// still perfectly promotable and must stay `Queued` for the report that closes
/// it. The whole startable set is read only by the dispatcher's startup and periodic
/// resync; between them it reads what moved, through [`find_startable_shared_builds_among`].
pub async fn find_startable_shared_builds<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<gradient_types::MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;
    gradient_types::EDerivationBuild::find()
        .from_raw_sql(FIND_STARTABLE_SHARED_BUILDS.stmt())
        .all(db)
        .await
}

/// The same gate over the shared builds of `derivations` alone: the startable-set moves
/// one pass admits, so its cost follows what moved, not what is queued.
pub async fn find_startable_shared_builds_among<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<gradient_types::MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    gradient_types::EDerivationBuild::find()
        .from_raw_sql(FIND_STARTABLE_SHARED_BUILDS_AMONG.bind([ids.into()]))
        .all(db)
        .await
}

fn startable_shared_builds_sql(scope: &str) -> String {
    let not_in_flight = crate::assignment_record::no_open_assignment_predicate(
        &crate::assignment_record::build_job_key_sql("db.id"),
    );

    format!(
        r#"
        SELECT db.*
        FROM derivation_build db
        WHERE db.status = {queued}
          {scope}
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

fn find_startable_shared_builds_sql() -> String {
    startable_shared_builds_sql("")
}

fn find_startable_shared_builds_among_sql() -> String {
    startable_shared_builds_sql("AND db.derivation = ANY($1::uuid[])")
}

crate::sql_fn! {
    FIND_STARTABLE_SHARED_BUILDS = find_startable_shared_builds_sql,
        params = [],
        tier = Bulk;

    FIND_STARTABLE_SHARED_BUILDS_AMONG = find_startable_shared_builds_among_sql,
        params = [DerivationIds(64)];
}

/// SQL predicate: the `derivation_build` aliased `alias` has a recorded
/// deterministic build failure - the builder ran and exited non-zero. Rebuilding
/// the identical derivation reproduces it, so a fresh evaluation must not thaw it
/// (else it loops: re-queue -> rebuild -> same non-zero exit -> re-queue). Only a
/// changed drv (a new shared build) or a output newly available in a cache can recover it.
fn deterministic_build_failure(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM build_attempt ba WHERE ba.derivation_build = {alias}.id \
         AND ba.outcome = {outcome} AND ba.reason = {reason})",
        outcome = status_sql::attempt_outcome(AttemptOutcome::Failed),
        reason = status_sql::attempt_reason(AttemptFailureReason::BuilderNonzero),
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
/// `work_mem` [`crate::graph_sql::begin_walk`] sets, so it has to run inside
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
        let walk = crate::graph_sql::begin_walk(db).await?;
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

fn requeue_failed_shared_builds_sql() -> String {
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
        created = status_sql::build(BuildStatus::Created),
        requeueable = status_sql::build_in(&BuildStatus::REQUEUEABLE),
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
    let walk = crate::graph_sql::begin_walk(db).await?;
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
        created = status_sql::build(BuildStatus::Created),
        requeueable = status_sql::build_in(&BuildStatus::REQUEUEABLE),
    )
}

fn requeue_failed_closure_fresh_sql() -> String {
    format!("{}\n{}", eval_closure_cte(), requeue_closure_update(""))
}

fn requeue_failed_closure_blocked_sql() -> String {
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

/// Repair shared build state from cache state across an evaluation's dependency
/// closure: any shared build whose outputs are **all** present in our cache
/// (`cached_path.file_hash`) is marked `Completed`, even if a
/// requeue / dependency-failed cascade / demote previously reset it. The dispatch
/// gate keys on the build-graph shared build state, which repeatedly desyncs from the
/// durable cache state - a derivation whose artifacts exist sits `Created` and
/// blocks its parents with nothing to build. Cache presence is the ground truth
/// for "is this built", so trust it here; the reactive heals
/// ([`crate::demote_parents_of`] / [`crate::demote_output_only_cached_deps`])
/// remain the backstop for the rare case where a cached output's runtime closure is
/// itself incomplete. Returns the changes it made, so the caller can advance the
/// parents of what it just settled; a shared build already terminal-success is left
/// alone, since it has nothing left for this statement to write.
pub async fn repair_cached_shared_builds_for_eval<C>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(
            REPAIR_CACHED_SHARED_BUILDS_FOR_EVAL.bind([Value::Uuid(Some(evaluation.into_inner()))]),
        )
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

fn repair_cached_shared_builds_for_eval_sql() -> String {
    let cte = eval_closure_cte();
    let not_in_flight = crate::assignment_record::no_open_assignment_predicate(
        &crate::assignment_record::build_job_key_sql("db.id"),
    );
    format!(
        r#"
    {cte}
    UPDATE derivation_build db
    SET status = CASE WHEN db.status IN ({terminal_success}) THEN db.status ELSE {completed} END,
        updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.derivation IN (SELECT derivation FROM closure)
      AND db.status NOT IN ({terminal_success})
      AND {not_in_flight}
      AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation)
      AND NOT EXISTS (
        SELECT 1 FROM derivation_output o
        LEFT JOIN cached_path cp ON cp.hash = o.hash AND cp.file_hash IS NOT NULL
        WHERE o.derivation = db.derivation AND cp.hash IS NULL)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        terminal_success = status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS),
        completed = status_sql::build(BuildStatus::Completed),
    )
}

crate::sql_fn! {
    REPAIR_CACHED_SHARED_BUILDS_FOR_EVAL = repair_cached_shared_builds_for_eval_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, plus \
                      one output-and-path anti-join per shared build the closure names"),
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
                outcome = status_sql::attempt_outcome(AttemptOutcome::Failed),
                reason = status_sql::attempt_reason(AttemptFailureReason::BuilderNonzero),
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
                status_sql::build_in(&BuildStatus::REQUEUEABLE)
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
                    status_sql::build_in(&BuildStatus::REQUEUEABLE)
                )),
                "still thaws the requeueable states for transient causes: {sql}"
            );
        }
    }

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
                terminal_failure = status_sql::build_in(&BuildStatus::TERMINAL_FAILURE),
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

    /// Dispatch trusts the queue invariant: no can-start state term is re-evaluated
    /// per row here, only the status and the reachability check.
    #[test]
    fn assign_reads_the_queued_invariant_only() {
        let sql = find_startable_shared_builds_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(sql.contains(&format!(
            "db.status = {}",
            status_sql::build(BuildStatus::Queued)
        )));
        assert!(sql.contains("FROM build_job bj WHERE bj.derivation = db.derivation"));
        assert!(
            !sql.contains("blocking_deps")
                && !sql.contains("fetchable")
                && !sql.contains("cached_path"),
            "{sql}"
        );
    }

    /// Cache presence is the ground truth for "built": a pending shared build whose
    /// outputs are complete in our cache is settled `Substituted` without a
    /// dispatch. Only `Created` moves, so a `Queued` shared build already in the
    /// tracker is not pulled out from under the dispatcher.
    #[test]
    fn substitute_created_shared_builds_moves_only_created_rows() {
        let sql = substitute_created_shared_builds_sql()
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
    fn assign_refuses_a_shared_build_whose_assignment_row_is_open() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let gate = norm(crate::assignment_record::no_open_assignment_predicate(
            &crate::assignment_record::build_job_key_sql("db.id"),
        ));

        assert!(norm(find_startable_shared_builds_sql()).contains(&gate));
    }

    /// A passthrough out on a worker settles its own shared build `Substituted`; the repair
    /// finding its outputs first called it built.
    #[test]
    fn the_cached_repair_leaves_a_shared_build_whose_assignment_row_is_open() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let gate = norm(crate::assignment_record::no_open_assignment_predicate(
            &crate::assignment_record::build_job_key_sql("db.id"),
        ));

        assert!(norm(repair_cached_shared_builds_for_eval_sql()).contains(&gate));
    }

    /// The delta is the resync's gate narrowed to what moved: a copy that
    /// drifted would admit a shared build the resync then prunes, or the reverse.
    #[test]
    fn the_delta_is_the_startable_set_narrowed_to_what_moved() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let among = norm(find_startable_shared_builds_among_sql());
        let scope = "AND db.derivation = ANY($1::uuid[]) ";

        assert!(among.contains(scope), "{among}");
        assert_eq!(
            among.replacen(scope, "", 1),
            norm(find_startable_shared_builds_sql())
        );
    }

    #[tokio::test]
    async fn no_moves_means_no_statement() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();

        let shared_builds = find_startable_shared_builds_among(&db, &[])
            .await
            .expect("no-op");

        assert!(shared_builds.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }
}
