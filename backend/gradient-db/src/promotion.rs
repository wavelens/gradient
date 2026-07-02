/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Graph-driven `Created -> Queued` promotion over the global
//! `derivation_dependency` graph. A derivation becomes buildable the moment
//! all its dependency anchors reach terminal-success - independent of any
//! single evaluation's completion (this replaces eval-completion-bound
//! promotion, the root cause of builds stuck in `Created`).
//!
//! Promotion is gated on reachability: an anchor is queued only while some
//! `build_job` references its derivation. The anchor table is global and
//! `derivation_build` rows are seeded for every derivation, so without this
//! gate promotion would queue derivations no surviving evaluation needs, which
//! the dispatcher then cannot attribute to a driving evaluation.
//!
//! # Derived-flag maintenance contract
//!
//! The gates in this module trust derived flags on `derivation_build`; each has
//! an explicit discipline, and mixing them up re-opens a dead-zone class:
//!
//! | flag                 | discipline                | heal                              |
//! |----------------------|---------------------------|-----------------------------------|
//! | `closure_complete`   | bidirectional (CLEAR+SET) | [`reconcile_closure_complete`]    |
//! | `drv_closure_cached` | bidirectional (CLEAR+SET) | [`reconcile_drv_closure_cached`]  |
//! | `edges_complete`     | monotonic (set-only)      | none needed - see below           |
//!
//! The two closure flags cache ground truth that can REGRESS (GC deletes a NAR,
//! an output is evicted, an edge is recorded late), so they must be cleared as
//! well as set - a stale-true flag dispatches a build whose inputs are gone,
//! the terminal-`InputsUnavailable` poison class.
//!
//! `edges_complete` is different: it records that the anchor's dependency EDGE
//! SET has been fully flushed by some evaluation, and that knowledge never
//! regresses - edges are only ever added, anchors die only by `derivation`
//! cascade, and nothing anywhere writes `edges_complete = false`. The one case
//! where a flushed edge set is UNTRUSTWORTHY (a declared dependency
//! `flush_deferred_deps` could not record) is held off promotion by the
//! separate `edges_unresolved` flag instead of a clear.

use crate::graph_sql::{ClosureDirection, dependency_closure_cte, eval_closure_cte};
use crate::status::TransitionChange;
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, QueryResult, Statement, Value};

// derivation_build.status numeric values: Created=0, Queued=1, Completed=3,
// FailedPermanent=4, DependencyFailed=6, Substituted=7, FailedTimeout=9.

/// Collect the `derivation` column of a `RETURNING derivation` result set. The
/// bulk transitions return the anchors they actually moved so the caller can fan
/// the CI status reactor out over exactly those (and only those) builds.
fn returned_derivations(rows: Vec<QueryResult>) -> Vec<DerivationId> {
    rows.into_iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

/// Collect `RETURNING db.derivation, old.status AS from_status, db.status AS
/// to_status` rows into the typed changes the effects emitter consumes. Bulk
/// statements capture the pre-update status via a `FROM derivation_build old`
/// self-join on the primary key (Postgres evaluates `old` against the snapshot).
fn returned_transitions(rows: Vec<QueryResult>) -> Vec<TransitionChange> {
    rows.into_iter()
        .filter_map(|r| {
            let derivation = r.try_get::<uuid::Uuid>("", "derivation").ok()?;
            let from = BuildStatus::try_from(r.try_get::<i32>("", "from_status").ok()?).ok()?;
            let to = BuildStatus::try_from(r.try_get::<i32>("", "to_status").ok()?).ok()?;
            Some(TransitionChange { derivation: DerivationId::new(derivation), from, to })
        })
        .collect()
}

/// Changes for rows a statement moved from a statically-known status (e.g. a
/// `WHERE status = 0` promote): no self-join needed, the predicate is the proof.
fn transitions_from(
    derivations: Vec<DerivationId>,
    from: BuildStatus,
    to: BuildStatus,
) -> Vec<TransitionChange> {
    derivations
        .into_iter()
        .map(|derivation| TransitionChange { derivation, from, to })
        .collect()
}

/// Re-evaluate the dependents of a just-finished `completed_derivation`:
/// mark any dependent with a terminal-failed dependency `DependencyFailed`,
/// then promote every `Created` dependent whose dependency anchors are all
/// terminal-success to `Queued`. Returns the changes it made so the caller can
/// feed [`crate::status::emit_transition_effects`].
pub async fn promote_dependents<C: ConnectionTrait>(
    db: &C,
    completed_derivation: DerivationId,
) -> Result<Vec<TransitionChange>, DbErr> {
    let id = || Value::Uuid(Some(Box::new(completed_derivation.into_inner())));

    let mut affected = returned_transitions(
        db.query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build AS db
            SET status = 6, updated_at = (now() AT TIME ZONE 'UTC')
            FROM derivation_build old
            WHERE old.id = db.id
              AND db.status IN (0, 1)
              AND db.derivation IN (
                SELECT dd.derivation FROM derivation_dependency dd WHERE dd.dependency = $1)
              AND EXISTS (
                SELECT 1 FROM derivation_dependency e
                JOIN derivation_build dep ON dep.derivation = e.dependency
                WHERE e.derivation = db.derivation AND dep.status IN (4, 6, 9))
            RETURNING db.derivation, old.status AS from_status, db.status AS to_status
            "#,
            [id()],
        ))
        .await?,
    );

    let deps_ready = crate::graph_sql::deps_ready_predicate("db");
    affected.extend(transitions_from(
        returned_derivations(
            db.query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    r#"
            UPDATE derivation_build AS db
            SET status = 1, queued_at = (now() AT TIME ZONE 'UTC'),
                updated_at = (now() AT TIME ZONE 'UTC')
            WHERE db.status = 0
              AND db.edges_complete
              AND db.derivation IN (
                SELECT dd.derivation FROM derivation_dependency dd WHERE dd.dependency = $1)
              AND EXISTS (
                SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation)
              AND (db.substitutable OR ({deps_ready}))
            RETURNING db.derivation
            "#
                ),
                [id()],
            ))
            .await?,
        ),
        BuildStatus::Created,
        BuildStatus::Queued,
    ));

    Ok(affected)
}

/// Closure-complete gate for a built anchor `db`: outputs cached, edges flushed,
/// and every build dependency itself `closure_complete` **or** `substitutable`.
/// Shared verbatim by the targeted up-ripple (`propagate_closure_complete`) and
/// the global self-heal fixpoint (`reconcile_closure_complete`).
pub(crate) const CLOSURE_COMPLETE_GATE: &str = r#"
    db.status = 3
    AND db.edges_complete
    AND NOT EXISTS (
        SELECT 1 FROM derivation_output o
        LEFT JOIN cached_path cp ON cp.hash = o.hash
        WHERE o.derivation = db.derivation AND cp.file_hash IS NULL)
    AND NOT EXISTS (
        SELECT 1 FROM derivation_dependency e
        LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
        WHERE e.derivation = db.derivation
          AND (dep.derivation IS NULL OR NOT (dep.closure_complete OR dep.substitutable)))
"#;

/// Recompute closure-completeness up the build-dependency graph from a just-
/// finished `completed` derivation. A built (`Completed`) anchor becomes
/// `closure_complete` once its outputs are cached, its edges are flushed, and
/// every build dependency is itself `closure_complete` **or** `substitutable`
/// (its closure is fetchable from upstream on demand). Substituted anchors are
/// not marked here - we hold only their output NAR, not their build closure, so
/// dependents reach them via the `substitutable` arm of the gate instead.
///
/// Marking ripples to dependents: completing one anchor can complete those that
/// were waiting only on it. This is the missing up-propagation - a dependent that
/// finished before its dependency did never re-evaluated its own completeness.
pub async fn propagate_closure_complete<C: ConnectionTrait>(
    db: &C,
    completed: DerivationId,
) -> Result<(), DbErr> {
    // Round-1 candidates: `completed` itself (it may now be closure_complete)
    // plus its direct dependents - a *substituted* `completed` is never marked
    // here, but it satisfies its dependents through the `substitutable` arm, so
    // they must still be re-checked even though `completed` never enters `newly`.
    let mut frontier = returned_derivations(
        db.query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT DISTINCT e.derivation FROM derivation_dependency e WHERE e.dependency = $1",
            [completed.into_inner().into()],
        ))
        .await?,
    );
    frontier.push(completed);
    let update = format!(
        "UPDATE derivation_build db SET closure_complete = true \
         WHERE db.derivation = ANY($1) AND NOT db.closure_complete AND {CLOSURE_COMPLETE_GATE} \
         RETURNING db.derivation"
    );
    while !frontier.is_empty() {
        let ids: Vec<uuid::Uuid> = frontier.iter().map(|d| d.into_inner()).collect();
        let newly = returned_derivations(
            db.query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &update,
                [ids.into()],
            ))
            .await?,
        );
        if newly.is_empty() {
            break;
        }

        let newly_ids: Vec<uuid::Uuid> = newly.iter().map(|d| d.into_inner()).collect();
        frontier = returned_derivations(
            db.query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT e.derivation FROM derivation_dependency e WHERE e.dependency = ANY($1)",
                [newly_ids.into()],
            ))
            .await?,
        );
    }
    Ok(())
}

/// Bidirectional self-heal fixpoint over `closure_complete`.
///
/// `propagate_closure_complete` only fires on a fresh completion event, so
/// anchors that completed under older code (e.g. before output-only
/// substitution) sit at `closure_complete = false` forever and strand their
/// dependents in `Created` with no error to trigger a reactive heal - the SET
/// pass below heals those.
///
/// The flag is otherwise monotonic, which is itself unsound: once true it
/// survives a closure member being demoted/evicted, or a dependency edge being
/// recorded after the fact (a dependent instantiated before its dependency).
/// The dispatch gate trusts `closure_complete` for direct deps, so a stale-true
/// flag dispatches a build whose transitive closure is not actually cached -
/// terminal `InputsUnavailable` on a tiny transitive output (e.g.
/// `unit-*.service`). The CLEAR pass restores soundness: any anchor whose gate
/// no longer holds (its output is uncached, a dependency regressed, or a newly
/// recorded dependency is not itself complete) is reset to false. Clearing
/// ripples up - a cleared dep fails its dependents' gate, cleared the next pass.
///
/// Run CLEAR to a fixpoint first (remove stale-true), then SET (mark genuinely
/// satisfied), each converging in O(longest affected chain). A converged graph
/// costs two zero-row statements.
pub async fn reconcile_closure_complete<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
    let clear = format!(
        "UPDATE derivation_build db SET closure_complete = false \
         WHERE db.closure_complete AND NOT ({CLOSURE_COMPLETE_GATE})"
    );
    loop {
        let changed = db
            .execute(Statement::from_string(DatabaseBackend::Postgres, &clear))
            .await?
            .rows_affected();
        if changed == 0 {
            break;
        }
    }

    let update = format!(
        "UPDATE derivation_build db SET closure_complete = true \
         WHERE NOT db.closure_complete AND {CLOSURE_COMPLETE_GATE}"
    );
    loop {
        let changed = db
            .execute(Statement::from_string(DatabaseBackend::Postgres, &update))
            .await?
            .rows_affected();
        if changed == 0 {
            break;
        }
    }

    Ok(())
}

/// `.drv`-closure gate for anchor `db`: its own `.drv` is cached (a `.drv`'s
/// store-path hash is the derivation hash) and every build dependency is itself
/// `drv_closure_cached`. The recursion mirrors `CLOSURE_COMPLETE_GATE` but tracks
/// the build-INPUT `.drv` closure instead of the OUTPUT closure, and is
/// independent of build/substitute status: a substitutable dependency's `.drv`
/// is still a structural reference of any dependent's `.drv` and so must be
/// cached for the dependent's import to succeed.
pub(crate) const DRV_CLOSURE_CACHED_GATE: &str = r#"
    db.edges_complete
    AND EXISTS (
        SELECT 1 FROM derivation d
        JOIN cached_path cp ON cp.hash = d.hash
        WHERE d.id = db.derivation AND cp.file_hash IS NOT NULL)
    AND NOT EXISTS (
        SELECT 1 FROM derivation_dependency e
        LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
        WHERE e.derivation = db.derivation
          AND (dep.derivation IS NULL OR NOT dep.drv_closure_cached))
"#;

/// CLEAR + SET statements for the `drv_closure_cached` fixpoint, sharing
/// `DRV_CLOSURE_CACHED_GATE` so both passes key on the same `.drv`-cached ground
/// truth and can never drift from each other (or from the test that pins them).
fn drv_closure_cached_statements() -> (String, String) {
    let clear = format!(
        "UPDATE derivation_build db SET drv_closure_cached = false \
         WHERE db.drv_closure_cached AND NOT ({DRV_CLOSURE_CACHED_GATE})"
    );
    let set = format!(
        "UPDATE derivation_build db SET drv_closure_cached = true \
         WHERE NOT db.drv_closure_cached AND {DRV_CLOSURE_CACHED_GATE}"
    );
    (clear, set)
}

/// Bidirectional self-heal fixpoint over `drv_closure_cached`, the dispatch gate's
/// ".drv closure is importable" trust flag. The eval pushes `.drv`s progressively,
/// so the SET pass marks anchors whose full input-`.drv` closure has landed - a
/// layer per pass, a freshly marked dep unblocking its dependents next pass.
///
/// The flag is not monotonic-safe: GC deletes a `.drv`'s `cached_path` row once
/// its NAR object is gone (`purge_zombie_cached_paths`), and the post-GC
/// `demote_unbacked_trusted_outputs` backstop only heals OUTPUT trust, never this
/// INPUT flag. A stale-true `drv_closure_cached` then dispatches a build whose
/// `.drv` is not actually cached - terminal `InputsUnavailable` on the build's own
/// `.drv`, poisoning the whole dependent closure. The CLEAR pass restores
/// soundness: any anchor whose `.drv` is no longer backed (or whose dependency
/// regressed) is reset, rippling up to dependents. Run CLEAR to a fixpoint first,
/// then SET; a converged graph costs two zero-row statements.
pub async fn reconcile_drv_closure_cached<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
    let (clear, set) = drv_closure_cached_statements();
    for stmt in [clear, set] {
        loop {
            let changed = db
                .execute(Statement::from_string(DatabaseBackend::Postgres, &stmt))
                .await?
                .rows_affected();
            if changed == 0 {
                break;
            }
        }
    }

    Ok(())
}

/// Recursively mark every dependent of `failed_derivation` `DependencyFailed`.
/// Walks the global `derivation_dependency` graph upward: any non-terminal
/// anchor (`Created`/`Queued`/`FailedTransient`) reachable from the failure can
/// never build, so it is failed in one recursive statement. Returns the changes
/// it made so the caller can feed [`crate::status::emit_transition_effects`].
pub async fn cascade_dependency_failed<C: ConnectionTrait>(
    db: &C,
    failed_derivation: DerivationId,
) -> Result<Vec<TransitionChange>, DbErr> {
    let cte = dependency_closure_cte("dependents", "SELECT $1::uuid", ClosureDirection::Dependents);
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                r#"
            {cte}
            UPDATE derivation_build AS db
            SET status = 6, updated_at = (now() AT TIME ZONE 'UTC')
            FROM derivation_build old
            WHERE old.id = db.id
              AND db.status IN (0, 1, 8)
              AND db.derivation IN (SELECT derivation FROM dependents WHERE derivation <> $1)
            RETURNING db.derivation, old.status AS from_status, db.status AS to_status
            "#
            ),
            [Value::Uuid(Some(Box::new(failed_derivation.into_inner())))],
        ))
        .await?;

    Ok(returned_transitions(rows))
}

/// Global proactive mirror of [`cascade_dependency_failed`]. The reactive cascade
/// fires only on a fresh terminal-failure *transition*, so it cannot reach an
/// anchor that becomes non-terminal **after** its dependency already failed:
/// `requeue_failed_anchors` / `requeue_failed_closure_for_eval` thaw a dependent
/// back to `Created` without re-checking its (still-failed) dependency, and a
/// concurrent eval can re-fail a dependency after the dependent was thawed. Such a
/// dependent can never build, yet sits `Created`/`Queued`/`FailedTransient`
/// forever - the dispatch gate holds it (its dep is not terminal-success) and
/// `check_evaluation_done` never finalizes its evaluation. This sweep walks
/// `derivation_dependency` upward from every terminal-failed anchor and fails each
/// reachable non-terminal anchor in one statement (the recursive term traverses
/// the graph structurally, so a whole poisoned subtree converges per pass). It is
/// the failure-side counterpart of the [`promote_ready`] success-side backstop.
/// Returns the changes it made so the caller can fan out the effects and
/// finalize the now-settled evaluations.
pub async fn reconcile_dependency_failed<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<TransitionChange>, DbErr> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            dependency_failed_reconcile_sql(),
        ))
        .await?;

    Ok(returned_transitions(rows))
}

/// Recursive upward walk from every terminal-failed anchor (`FailedPermanent=4`/
/// `DependencyFailed=6`/`FailedTimeout=9`) that fails each reachable non-terminal
/// anchor (`Created=0`/`Queued=1`/`FailedTransient=8`). Mirrors the reactive
/// [`cascade_dependency_failed`] terminal-failed set (it excludes `Aborted=5`,
/// which is retried, not permanent). The failed roots are excluded from the UPDATE
/// by the `status IN (0, 1, 8)` predicate, so the sweep is idempotent.
fn dependency_failed_reconcile_sql() -> String {
    let cte = dependency_closure_cte(
        "dependents",
        "SELECT derivation FROM derivation_build WHERE status IN (4, 6, 9)",
        ClosureDirection::Dependents,
    );
    format!(
        r#"
    {cte}
    UPDATE derivation_build AS db
    SET status = 6, updated_at = (now() AT TIME ZONE 'UTC')
    FROM derivation_build old
    WHERE old.id = db.id
      AND db.status IN (0, 1, 8)
      AND db.derivation IN (SELECT derivation FROM dependents)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#
    )
}

/// Promote every `Created` anchor whose dependency anchors are all terminal-
/// success (`Completed`/`Substituted`) to `Queued`. Run once an evaluation's
/// full dependency graph is written (edges are deferred to stream completion):
/// this seeds the graph from its leaves and from anchors whose deps were already
/// cached/substituted at resolve time (for which no completion event ever
/// fires). Subsequent completions cascade via [`promote_dependents`]. Returns
/// the changes it made so the caller can feed the effects emitter.
pub async fn promote_ready<C: ConnectionTrait>(db: &C) -> Result<Vec<TransitionChange>, DbErr> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            promote_ready_sql(),
        ))
        .await?;

    Ok(transitions_from(
        returned_derivations(rows),
        BuildStatus::Created,
        BuildStatus::Queued,
    ))
}

fn promote_ready_sql() -> String {
    let deps_ready = crate::graph_sql::deps_ready_predicate("db");
    format!(
        r#"
        UPDATE derivation_build AS db
        SET status = 1, queued_at = (now() AT TIME ZONE 'UTC'),
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.status = 0
          AND db.edges_complete
          AND EXISTS (
            SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation)
          AND (db.substitutable OR ({deps_ready}))
        RETURNING db.derivation
        "#
    )
}

/// The dispatch gate: every `Queued` anchor whose inputs are genuinely present
/// right now. Reachability (`build_job` EXISTS) skips anchors left Queued after
/// their last referencing eval was torn down; a `substitutable` anchor dispatches
/// with no dependency wait at all (its NAR is on an upstream cache); otherwise the
/// anchor's own `.drv` closure must be importable (`drv_closure_cached`) and the
/// shared readiness predicate must hold. Ordered by dependency count desc
/// (integration builds first), then age. This is [`promote_ready`]'s predicate
/// applied one step later - both embed [`crate::graph_sql::deps_ready_predicate`].
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
    let deps_ready = crate::graph_sql::deps_ready_predicate("db");
    format!(
        r#"
        SELECT db.*
        FROM derivation_build db
        WHERE db.status = 1
          AND db.edges_complete
          AND EXISTS (
            SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation)
          AND (db.substitutable OR (db.drv_closure_cached AND {deps_ready}))
        ORDER BY
            (SELECT count(*)
               FROM derivation_dependency dd
              WHERE dd.derivation = db.derivation) DESC,
            db.updated_at ASC
        "#
    )
}

/// Mark `edges_complete` across `evaluation`'s full build-dependency closure, not
/// just its directly-reported `build_job` rows. Called once the eval's dependency
/// edges are flushed. A transitive dep reached only via global edges (pruned or
/// substituted in this eval, so it has no `build_job` here) would otherwise never
/// get its flag maintained: if the eval that owned it never completed its edge
/// flush (failed, interrupted, superseded), the dep sits `edges_complete = false`
/// forever - unpromotable behind the dispatch gate even though its edge set is by
/// now complete and satisfied. A closure node is marked when it
/// has recorded build edges (its edge set is known) or is one of this eval's own
/// `build_job` leaves (0-dep); ambiguous 0-edge transitive nodes stay gated.
/// Anchors flagged `edges_unresolved` (a declared dependency `flush_deferred_deps`
/// could not record) are never marked, so a build_job whose edges were dropped is
/// held instead of dispatched as dependency-free. Idempotent and never clears it.
pub async fn mark_edges_complete_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<u64, DbErr> {
    let cte = eval_closure_cte();
    let affected = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                r#"
            {cte}
            UPDATE derivation_build db
            SET edges_complete = true
            WHERE db.edges_complete = false
              AND NOT db.edges_unresolved
              AND db.derivation IN (SELECT derivation FROM closure)
              AND (
                EXISTS (SELECT 1 FROM derivation_dependency e WHERE e.derivation = db.derivation)
                OR EXISTS (SELECT 1 FROM build_job bj
                           WHERE bj.derivation = db.derivation AND bj.evaluation = $1)
              )
            "#
            ),
            [Value::Uuid(Some(Box::new(evaluation.into_inner())))],
        ))
        .await?
        .rows_affected();

    Ok(affected)
}

/// Re-queue anchors a previous evaluation left in a terminal-failure state
/// (`FailedPermanent`/`Aborted`/`DependencyFailed`/`FailedTimeout`) back to
/// `Created`, for the derivations a new evaluation needs. A new evaluation is a
/// fresh build intent - the upstream cache, network, or a transient cause may
/// have changed since the global anchor failed - so it retries rather than
/// inheriting the stale failure. Build-once success states
/// (`Completed`/`Substituted`) are never touched. Returns the number re-queued.
pub async fn requeue_failed_anchors<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<u64, DbErr> {
    let mut total = 0;
    for chunk in derivations.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        total += db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                UPDATE derivation_build
                SET status = 0, attempt = 0, closure_complete = false,
                    updated_at = (now() AT TIME ZONE 'UTC')
                WHERE derivation = ANY($1) AND status IN (4, 5, 6, 9)
                "#,
                [ids.into()],
            ))
            .await?
            .rows_affected();
    }

    Ok(total)
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
/// can rebuild the failed subtree bottom-up. Returns the number re-queued.
pub async fn requeue_failed_closure_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<u64, DbErr> {
    let cte = eval_closure_cte();
    let affected = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                r#"
            {cte}
            UPDATE derivation_build db
            SET status = 0, attempt = 0, closure_complete = false,
                updated_at = (now() AT TIME ZONE 'UTC')
            WHERE db.derivation IN (SELECT derivation FROM closure)
              AND db.status IN (4, 5, 6, 9)
            "#
            ),
            [Value::Uuid(Some(Box::new(evaluation.into_inner())))],
        ))
        .await?
        .rows_affected();

    Ok(affected)
}

/// Reconcile anchor state from cache state across an evaluation's dependency
/// closure: any anchor whose outputs are **all** present in our cache
/// (`cached_path.file_hash`) is marked `Completed` + `closure_complete`, even if a
/// requeue / dependency-failed cascade / demote previously reset it. The dispatch
/// gate keys on the build-graph anchor state, which repeatedly desyncs from the
/// durable cache state - a derivation whose artifacts exist sits `Created` and
/// blocks its dependents with nothing to build. Cache presence is the ground truth
/// for "is this built", so trust it here; the reactive heals
/// (`demote_referrers_of` / absent-orphan recovery) remain the backstop for the
/// rare case where a cached output's runtime closure is itself incomplete. Returns
/// the changes it made (flag-only touches on already-terminal anchors report
/// `from == to`, which the effects emitter treats as a re-announce).
pub async fn reconcile_cached_anchors_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr> {
    let cte = eval_closure_cte();
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                r#"
            {cte}
            UPDATE derivation_build db
            SET status = CASE WHEN db.status IN (3, 7) THEN db.status ELSE 3 END,
                closure_complete = true,
                edges_complete = true,
                updated_at = (now() AT TIME ZONE 'UTC')
            FROM derivation_build old
            WHERE old.id = db.id
              AND db.derivation IN (SELECT derivation FROM closure)
              AND (db.status NOT IN (3, 7) OR NOT db.closure_complete)
              AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation)
              AND NOT EXISTS (
                SELECT 1 FROM derivation_output o
                LEFT JOIN cached_path cp ON cp.hash = o.hash AND cp.file_hash IS NOT NULL
                WHERE o.derivation = db.derivation AND cp.hash IS NULL)
            RETURNING db.derivation, old.status AS from_status, db.status AS to_status
            "#
            ),
            [Value::Uuid(Some(Box::new(evaluation.into_inner())))],
        ))
        .await?;

    Ok(returned_transitions(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The proactive dependency-failed sweep must mirror the reactive cascade: seed
    /// the recursive walk from the terminal-failed set the cascade reacts to
    /// (`FailedPermanent=4`/`DependencyFailed=6`/`FailedTimeout=9`, NOT `Aborted=5`),
    /// fail only non-terminal anchors (`Created=0`/`Queued=1`/`FailedTransient=8`)
    /// to `DependencyFailed=6`, and walk dependents upward via the dependency edge.
    /// Getting the seed or target set wrong either misses the dead zone or clobbers
    /// terminal-success anchors, so pin the SQL shape (no live DB in unit tests).
    #[test]
    fn dependency_failed_reconcile_sql_mirrors_the_cascade() {
        let sql = dependency_failed_reconcile_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            sql.contains("FROM derivation_build WHERE status IN (4, 6, 9)"),
            "must seed from the terminal-failed set (excluding Aborted=5): {sql}"
        );
        assert!(
            sql.contains("SET status = 6"),
            "must fail dependents to DependencyFailed: {sql}"
        );
        assert!(
            sql.contains("db.status IN (0, 1, 8)"),
            "must only touch non-terminal anchors (never terminal-success): {sql}"
        );
        assert!(
            sql.contains("FROM derivation_build old") && sql.contains("old.status AS from_status"),
            "must capture the pre-update status for the effects emitter: {sql}"
        );
        assert!(
            sql.contains("JOIN dependents c ON e.dependency = c.derivation"),
            "must walk dependents upward via the dependency edge: {sql}"
        );
        assert!(
            sql.contains("RETURNING db.derivation"),
            "must return failed derivations so the caller can finalize their evals: {sql}"
        );
    }

    /// Promotion and the dispatch gate must share one readiness definition: both
    /// statements embed `deps_ready_predicate` verbatim, and only the dispatch
    /// gate adds the `.drv`-importability arm (`drv_closure_cached`). A drift
    /// between the two is a latent dead zone (promoted but never dispatchable,
    /// or dispatched without its inputs).
    #[test]
    fn promotion_and_dispatch_share_the_readiness_predicate() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let predicate = norm(crate::graph_sql::deps_ready_predicate("db"));
        let promote = norm(promote_ready_sql());
        let dispatch = norm(find_ready_anchors_sql());
        assert!(promote.contains(&predicate), "promote_ready must embed the shared predicate: {promote}");
        assert!(dispatch.contains(&predicate), "find_ready_anchors must embed the shared predicate: {dispatch}");
        assert!(
            dispatch.contains("db.substitutable OR (db.drv_closure_cached AND"),
            "dispatch additionally requires an importable .drv closure: {dispatch}"
        );
        assert!(
            promote.contains("db.substitutable OR (NOT EXISTS"),
            "promotion must not gate on drv_closure_cached (the eval pushes .drvs progressively): {promote}"
        );
    }

    /// `drv_closure_cached` is the dispatch gate's ".drv closure is importable"
    /// trust flag. GC deletes a `.drv`'s `cached_path` row once its NAR object
    /// goes missing (`purge_zombie_cached_paths`), so the flag must be
    /// BIDIRECTIONAL like `closure_complete`: CLEAR a stale-true flag whose `.drv`
    /// is no longer backed before SETting genuinely satisfied anchors. A set-only
    /// reconcile leaves the gate trusting a vanished `.drv`, stranding the build in
    /// a terminal `InputsUnavailable` dead zone. Pin the SQL shape (no live DB).
    #[test]
    fn drv_closure_cached_reconcile_is_bidirectional() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let (clear, set) = drv_closure_cached_statements();
        let (clear, set) = (norm(clear), norm(set));
        assert!(
            clear.contains("SET drv_closure_cached = false")
                && clear.contains("WHERE db.drv_closure_cached AND NOT ("),
            "CLEAR pass must reset anchors whose .drv is no longer backed: {clear}"
        );
        assert!(
            set.contains("SET drv_closure_cached = true")
                && set.contains("WHERE NOT db.drv_closure_cached AND"),
            "SET pass must mark anchors whose .drv closure has landed: {set}"
        );
        assert!(
            clear.contains("JOIN cached_path cp") && set.contains("JOIN cached_path cp"),
            "both passes must key on real .drv NAR backing (ground truth): {clear} | {set}"
        );
    }
}
