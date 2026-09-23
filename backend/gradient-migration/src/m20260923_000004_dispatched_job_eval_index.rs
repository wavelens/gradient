/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The evaluation list links each evaluation to its eval job on the Job Board.
//! Eval jobs (`kind = 0`) are a sliver of `dispatched_job`, so the index covers only them.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-dispatched_job-eval-by-evaluation"
     ON dispatched_job (evaluation_id, dispatched_at DESC)
     WHERE kind = 0"#,
];

const DOWN: &[&str] = &[r#"DROP INDEX IF EXISTS "idx-dispatched_job-eval-by-evaluation""#];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in UP {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in DOWN {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
