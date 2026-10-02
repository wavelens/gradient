/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The new columns are deliberately not backfilled. `AVG` is skipping nulls like it skipped an
//! absent json key, and every window is at most 24 hours long.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE dispatched_job \
     ADD COLUMN IF NOT EXISTS missing_nar_size bigint, \
     ADD COLUMN IF NOT EXISTS missing_count integer, \
     ADD COLUMN IF NOT EXISTS dependency_count integer",
    "CREATE INDEX IF NOT EXISTS \"idx-dispatched_job-build-window\" \
     ON dispatched_job (dispatched_at) \
     INCLUDE (ready_at, missing_nar_size, missing_count, dependency_count) \
     WHERE kind = 1 AND ready_at IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS \"idx-build_attempt-latest\" \
     ON build_attempt (derivation_build, created_at DESC)",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-build_attempt-latest\"",
    "DROP INDEX IF EXISTS \"idx-dispatched_job-build-window\"",
    "ALTER TABLE dispatched_job \
     DROP COLUMN IF EXISTS missing_nar_size, \
     DROP COLUMN IF EXISTS missing_count, \
     DROP COLUMN IF EXISTS dependency_count",
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
