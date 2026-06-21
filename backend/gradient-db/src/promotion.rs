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
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement, Value};

// derivation_build.status numeric values: Created=0, Queued=1, Completed=3,
// FailedPermanent=4, DependencyFailed=6, Substituted=7, FailedTimeout=9.

/// Re-evaluate the dependents of a just-finished `completed_derivation`:
/// mark any dependent with a terminal-failed dependency `DependencyFailed`,
/// then promote every `Created` dependent whose dependency anchors are all
/// terminal-success to `Queued`. Returns the number of rows changed.
pub async fn promote_dependents<C: ConnectionTrait>(
    db: &C,
    completed_derivation: DerivationId,
) -> Result<u64, DbErr> {
    let id = || Value::Uuid(Some(Box::new(completed_derivation.into_inner())));

    let failed = db
        .execute(Statement::from_sql_and_values(
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
            "#,
            [id()],
        ))
        .await?
        .rows_affected();

    let queued = db
        .execute(Statement::from_sql_and_values(
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
              AND NOT EXISTS (
                SELECT 1 FROM derivation_dependency e
                LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
                WHERE e.derivation = db.derivation
                  AND (dep.status IS NULL OR dep.status NOT IN (3, 7)))
            "#,
            [id()],
        ))
        .await?
        .rows_affected();

    Ok(failed + queued)
}

/// Recursively mark every dependent of `failed_derivation` `DependencyFailed`.
/// Walks the global `derivation_dependency` graph upward: any non-terminal
/// anchor (`Created`/`Queued`/`FailedTransient`) reachable from the failure can
/// never build, so it is failed in one recursive statement. Returns rows changed.
pub async fn cascade_dependency_failed<C: ConnectionTrait>(
    db: &C,
    failed_derivation: DerivationId,
) -> Result<u64, DbErr> {
    let affected = db
        .execute(Statement::from_sql_and_values(
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
            "#,
            [Value::Uuid(Some(Box::new(failed_derivation.into_inner())))],
        ))
        .await?
        .rows_affected();

    Ok(affected)
}

/// Promote every `Created` anchor whose dependency anchors are all terminal-
/// success (`Completed`/`Substituted`) to `Queued`. Run once an evaluation's
/// full dependency graph is written (edges are deferred to stream completion):
/// this seeds the graph from its leaves and from anchors whose deps were already
/// cached/substituted at resolve time (for which no completion event ever
/// fires). Subsequent completions cascade via [`promote_dependents`]. Returns
/// the number promoted.
pub async fn promote_ready<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let affected = db
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build
            SET status = 1, queued_at = (now() AT TIME ZONE 'UTC'),
                updated_at = (now() AT TIME ZONE 'UTC')
            WHERE status = 0
              AND edges_complete
              AND EXISTS (
                SELECT 1 FROM build_job bj WHERE bj.derivation = derivation_build.derivation)
              AND NOT EXISTS (
                SELECT 1 FROM derivation_dependency e
                LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
                WHERE e.derivation = derivation_build.derivation
                  AND (dep.status IS NULL OR dep.status NOT IN (3, 7)))
            "#
            .to_string(),
        ))
        .await?
        .rows_affected();

    Ok(affected)
}

/// Mark every anchor reachable from `evaluation`'s `build_job` rows
/// `edges_complete`. Called once the eval's dependency edges are flushed, so its
/// anchors become eligible for promotion. Idempotent and never clears the flag:
/// edges are content-addressed and permanent once written, so a later requeue
/// keeps the anchor promotable without re-evaluation.
pub async fn mark_edges_complete_for_eval<C: ConnectionTrait>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<u64, DbErr> {
    let affected = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build
            SET edges_complete = true
            WHERE edges_complete = false
              AND derivation IN (
                SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1)
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
                SET status = 0, attempt = 0, updated_at = (now() AT TIME ZONE 'UTC')
                WHERE derivation = ANY($1) AND status IN (4, 5, 6, 9)
                "#,
                [ids.into()],
            ))
            .await?
            .rows_affected();
    }

    Ok(total)
}
