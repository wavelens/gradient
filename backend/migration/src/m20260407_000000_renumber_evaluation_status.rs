/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Renumber `evaluation.status` values to accommodate three new statuses.
//!
//! Old layout (gaps added for clarity):
//!   0 Queued | 1 Evaluating | 2 Building | 3 Completed | 4 Failed | 5 Aborted
//!
//! New layout:
//!   0 Queued | 1 EvaluatingFlake | 2 EvaluatingDerivation | 3 Building
//!   | 4 Waiting | 5 Completed | 6 Failed | 7 Aborted
//!
//! Migration strategy (executed high→low to avoid transient collisions):
//!   old 5 Aborted    → 7
//!   old 4 Failed     → 6
//!   old 3 Completed  → 5
//!   old 2 Building   → 3
//!   old 1 Evaluating → 1 (EvaluatingFlake) — same numeric value, no-op

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260407_000000_renumber_evaluation_status"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Order matters: shift highest values first to avoid collisions.
        db.execute_unprepared("UPDATE evaluation SET status = 7 WHERE status = 5")
            .await?;
        db.execute_unprepared("UPDATE evaluation SET status = 6 WHERE status = 4")
            .await?;
        db.execute_unprepared("UPDATE evaluation SET status = 5 WHERE status = 3")
            .await?;
        db.execute_unprepared("UPDATE evaluation SET status = 3 WHERE status = 2")
            .await?;
        // old 1 (Evaluating) → 1 (EvaluatingFlake): same value, intentional no-op.
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Reverse: shift lowest values first.
        db.execute_unprepared("UPDATE evaluation SET status = 2 WHERE status = 3")
            .await?;
        db.execute_unprepared("UPDATE evaluation SET status = 3 WHERE status = 5")
            .await?;
        db.execute_unprepared("UPDATE evaluation SET status = 4 WHERE status = 6")
            .await?;
        db.execute_unprepared("UPDATE evaluation SET status = 5 WHERE status = 7")
            .await?;
        // Rows with status 2 (EvaluatingDerivation) or 4 (Waiting) didn't exist before;
        // map them back to 1 (Evaluating) as a best-effort.
        db.execute_unprepared("UPDATE evaluation SET status = 1 WHERE status = 2 OR status = 4")
            .await?;
        Ok(())
    }
}
