/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Replace `evaluation.repo_check_id` (single BigInt) with `check_run_ids`
//! (JSONB map keyed by check name). Per-evaluation reporting now creates
//! separate forge check runs for the Awaiting-Approval, Evaluation, and
//! per-Build phases; each needs its own check id so update PATCHes don't
//! collide.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        db.execute(Statement::from_string(
            backend,
            r#"ALTER TABLE evaluation
               ADD COLUMN check_run_ids JSONB"#,
        ))
        .await?;

        // Existing `repo_check_id` values are not migrated: the legacy column
        // stored a single id used for every phase, while the new layout keys
        // by the per-phase context name (e.g. `gradient/foo: Evaluation`).
        // In-flight PRs at deploy time will see a single duplicate check
        // appear and then heal naturally as the new phase reporters fire.
        db.execute(Statement::from_string(
            backend,
            r#"ALTER TABLE evaluation DROP COLUMN repo_check_id"#,
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        db.execute(Statement::from_string(
            backend,
            r#"ALTER TABLE evaluation ADD COLUMN repo_check_id BIGINT"#,
        ))
        .await?;
        db.execute(Statement::from_string(
            backend,
            r#"ALTER TABLE evaluation DROP COLUMN check_run_ids"#,
        ))
        .await?;

        Ok(())
    }
}
