/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds two indexes that support `dispatch_ready_builds`:
//!
//! 1. `idx-build-evaluation-derivation` — composite on `(evaluation,
//!    derivation)`. The dispatcher's `NOT EXISTS` antijoin correlates each
//!    candidate's dependency edges back to `build` rows in the same
//!    evaluation; without this index every dependency lookup degenerates
//!    into a sequential scan of `build`.
//!
//! 2. `idx-build-ready-queue` — partial index `(updated_at) WHERE status = 1
//!    AND via IS NULL`. The dispatcher only ever drives off Queued primary
//!    builds; this lets the planner walk the queue directly in `updated_at`
//!    order instead of full-scanning `build` to filter.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .name("idx-build-evaluation-derivation")
                    .table(Build::Table)
                    .col(Build::Evaluation)
                    .col(Build::Derivation)
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE INDEX "idx-build-ready-queue"
                   ON build (updated_at)
                   WHERE status = 1 AND via IS NULL"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP INDEX IF EXISTS "idx-build-ready-queue""#)
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx-build-evaluation-derivation")
                    .table(Build::Table)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Evaluation,
    Derivation,
}
