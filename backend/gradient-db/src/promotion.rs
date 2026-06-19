/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Graph-driven `Created -> Queued` promotion over the global
//! `derivation_dependency` graph. A derivation becomes buildable the moment
//! all its dependency anchors reach terminal-success - independent of any
//! evaluation's lifecycle (this replaces eval-completion-bound promotion, the
//! root cause of builds stuck in `Created`).

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
              AND db.derivation IN (
                SELECT dd.derivation FROM derivation_dependency dd WHERE dd.dependency = $1)
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

/// Promote leaf anchors (no dependency edges) `Created -> Queued`. Called after
/// each resolve batch so source / fixed-output derivations become buildable
/// immediately. Returns the number promoted.
pub async fn promote_leaves<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let affected = db
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build
            SET status = 1, queued_at = (now() AT TIME ZONE 'UTC'),
                updated_at = (now() AT TIME ZONE 'UTC')
            WHERE status = 0
              AND NOT EXISTS (
                SELECT 1 FROM derivation_dependency dd
                WHERE dd.derivation = derivation_build.derivation)
            "#
            .to_string(),
        ))
        .await?
        .rows_affected();

    Ok(affected)
}
