/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `build_attempt.build_job` is a NO ACTION foreign key without an index, so
//! every cascaded `build_job` delete scanned the whole table (#629), and
//! `build_finished_at` is the window the duration rollup now seeds from.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-build_attempt-build_job"
       ON build_attempt (build_job) WHERE build_job IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-build_attempt-build_finished_at"
       ON build_attempt (build_finished_at) WHERE build_finished_at IS NOT NULL"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-build_attempt-build_job""#,
    r#"DROP INDEX IF EXISTS "idx-build_attempt-build_finished_at""#,
];

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
