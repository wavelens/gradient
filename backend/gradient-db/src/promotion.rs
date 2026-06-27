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

/// Re-evaluate the dependents of a just-finished `completed_derivation`:
/// mark any dependent with a terminal-failed dependency `DependencyFailed`,
/// then promote every `Created` dependent whose dependency anchors are all
/// terminal-success to `Queued`. Returns the derivations it moved (queued or
/// dependency-failed) so the caller can post their CI status.
pub async fn promote_dependents<C: ConnectionTrait>(
    db: &C,
    completed_derivation: DerivationId,
) -> Result<Vec<DerivationId>, DbErr> {
    let id = || Value::Uuid(Some(Box::new(completed_derivation.into_inner())));

    let mut affected = returned_derivations(
        db.query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build AS db
            SET status = 6, updated_at = (now() AT TIME ZONE 'UTC')
            WHERE db.status IN (0, 1)
              AND db.derivation IN (
                SELECT dd.derivation FROM derivation_dependency dd WHERE dd.dependency = $1)
              AND EXISTS (
                SELECT 1 FROM derivation_dependency e
                JOIN derivation_build dep ON dep.derivation = e.dependency
                WHERE e.derivation = db.derivation AND dep.status IN (4, 6, 9))
            RETURNING db.derivation
            "#,
            [id()],
        ))
        .await?,
    );

    affected.extend(returned_derivations(
        db.query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
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
              AND (
                db.substitutable
                OR (
                  NOT EXISTS (
                    SELECT 1 FROM derivation_dependency e
                    LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
                    WHERE e.derivation = db.derivation
                      AND (dep.status IS NULL
                           OR NOT (((dep.status IN (3, 7)) AND dep.closure_complete)
                                   OR dep.substitutable)))
                  AND NOT EXISTS (
                    SELECT 1 FROM derivation_input_source s
                    WHERE s.derivation = db.derivation
                      AND NOT EXISTS (
                        SELECT 1 FROM cached_path cp
                        WHERE cp.hash = s.hash AND cp.file_hash IS NOT NULL))
                ))
            RETURNING db.derivation
            "#,
            [id()],
        ))
        .await?,
    ));

    Ok(affected)
}

/// Closure-complete gate for a built anchor `db`: outputs cached, edges flushed,
/// and every build dependency itself `closure_complete` **or** `substitutable`.
/// Shared verbatim by the targeted up-ripple (`propagate_closure_complete`) and
/// the global self-heal fixpoint (`reconcile_closure_complete`).
const CLOSURE_COMPLETE_GATE: &str = r#"
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

/// Global self-heal fixpoint over `closure_complete`. `propagate_closure_complete`
/// only fires on a fresh completion event, so anchors that completed under older
/// code (e.g. before output-only substitution) sit at `closure_complete = false`
/// forever and strand their dependents in `Created` with no error to trigger a
/// reactive heal. Run at eval completion before promotion: each pass marks a
/// layer of satisfied anchors, a freshly marked dep unblocks its dependents next
/// pass, converging in O(longest unmarked chain). Converged graphs cost one
/// zero-row statement.
pub async fn reconcile_closure_complete<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
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

/// Recursively mark every dependent of `failed_derivation` `DependencyFailed`.
/// Walks the global `derivation_dependency` graph upward: any non-terminal
/// anchor (`Created`/`Queued`/`FailedTransient`) reachable from the failure can
/// never build, so it is failed in one recursive statement. Returns the
/// derivations it failed so the caller can post their CI status.
pub async fn cascade_dependency_failed<C: ConnectionTrait>(
    db: &C,
    failed_derivation: DerivationId,
) -> Result<Vec<DerivationId>, DbErr> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH RECURSIVE dependents AS (
                SELECT $1::uuid AS derivation
                UNION
                SELECT dd.derivation FROM derivation_dependency dd
                JOIN dependents dt ON dd.dependency = dt.derivation
            )
            UPDATE derivation_build AS db
            SET status = 6, updated_at = (now() AT TIME ZONE 'UTC')
            WHERE db.status IN (0, 1, 8)
              AND db.derivation IN (SELECT derivation FROM dependents WHERE derivation <> $1)
            RETURNING db.derivation
            "#,
            [Value::Uuid(Some(Box::new(failed_derivation.into_inner())))],
        ))
        .await?;

    Ok(returned_derivations(rows))
}

/// Promote every `Created` anchor whose dependency anchors are all terminal-
/// success (`Completed`/`Substituted`) to `Queued`. Run once an evaluation's
/// full dependency graph is written (edges are deferred to stream completion):
/// this seeds the graph from its leaves and from anchors whose deps were already
/// cached/substituted at resolve time (for which no completion event ever
/// fires). Subsequent completions cascade via [`promote_dependents`]. Returns
/// the derivations it queued so the caller can post their CI status.
pub async fn promote_ready<C: ConnectionTrait>(db: &C) -> Result<Vec<DerivationId>, DbErr> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build
            SET status = 1, queued_at = (now() AT TIME ZONE 'UTC'),
                updated_at = (now() AT TIME ZONE 'UTC')
            WHERE status = 0
              AND edges_complete
              AND EXISTS (
                SELECT 1 FROM build_job bj WHERE bj.derivation = derivation_build.derivation)
              AND (
                derivation_build.substitutable
                OR (
                  NOT EXISTS (
                    SELECT 1 FROM derivation_dependency e
                    LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
                    WHERE e.derivation = derivation_build.derivation
                      AND (dep.status IS NULL
                           OR NOT (((dep.status IN (3, 7)) AND dep.closure_complete)
                                   OR dep.substitutable)))
                  AND NOT EXISTS (
                    SELECT 1 FROM derivation_input_source s
                    WHERE s.derivation = derivation_build.derivation
                      AND NOT EXISTS (
                        SELECT 1 FROM cached_path cp
                        WHERE cp.hash = s.hash AND cp.file_hash IS NOT NULL))
                ))
            RETURNING derivation_build.derivation
            "#
            .to_string(),
        ))
        .await?;

    Ok(returned_derivations(rows))
}

/// Mark `edges_complete` across `evaluation`'s full build-dependency closure, not
/// just its directly-reported `build_job` rows. Called once the eval's dependency
/// edges are flushed. A transitive dep reached only via global edges (pruned or
/// substituted in this eval, so it has no `build_job` here) would otherwise never
/// get its flag maintained, so a prior demote that cleared it leaves the dep
/// `edges_complete = false` forever - unpromotable behind the dispatch gate even
/// though its edge set is complete and satisfied. A closure node is marked when it
/// has recorded build edges (its edge set is known) or is one of this eval's own
/// `build_job` leaves (0-dep); ambiguous 0-edge transitive nodes stay gated.
/// Anchors flagged `edges_unresolved` (a declared dependency `flush_deferred_deps`
/// could not record) are never marked, so a build_job whose edges were dropped is
/// held instead of dispatched as dependency-free. Idempotent and never clears it.
pub async fn mark_edges_complete_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<u64, DbErr> {
    let affected = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH RECURSIVE closure AS (
                SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1
                UNION
                SELECT e.dependency FROM derivation_dependency e
                JOIN closure c ON c.derivation = e.derivation
            )
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
            "#,
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
    let affected = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH RECURSIVE closure AS (
                SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1
                UNION
                SELECT e.dependency FROM derivation_dependency e
                JOIN closure c ON c.derivation = e.derivation
            )
            UPDATE derivation_build db
            SET status = 0, attempt = 0, closure_complete = false,
                updated_at = (now() AT TIME ZONE 'UTC')
            WHERE db.derivation IN (SELECT derivation FROM closure)
              AND db.status IN (4, 5, 6, 9)
            "#,
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
/// the number reconciled.
pub async fn reconcile_cached_anchors_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<u64, DbErr> {
    let affected = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH RECURSIVE closure AS (
                SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1
                UNION
                SELECT e.dependency FROM derivation_dependency e
                JOIN closure c ON c.derivation = e.derivation
            )
            UPDATE derivation_build db
            SET status = CASE WHEN db.status IN (3, 7) THEN db.status ELSE 3 END,
                closure_complete = true,
                edges_complete = true,
                updated_at = (now() AT TIME ZONE 'UTC')
            WHERE db.derivation IN (SELECT derivation FROM closure)
              AND (db.status NOT IN (3, 7) OR NOT db.closure_complete)
              AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation)
              AND NOT EXISTS (
                SELECT 1 FROM derivation_output o
                LEFT JOIN cached_path cp ON cp.hash = o.hash AND cp.file_hash IS NOT NULL
                WHERE o.derivation = db.derivation AND cp.hash IS NULL)
            "#,
            [Value::Uuid(Some(Box::new(evaluation.into_inner())))],
        ))
        .await?
        .rows_affected();

    Ok(affected)
}
