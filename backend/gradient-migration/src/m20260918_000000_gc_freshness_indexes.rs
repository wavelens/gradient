/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The GC's freshness seed. `gc`'s `fresh` CTE opens with the rows created
//! since the retention cutoff, and neither table it reads had an index on
//! `created_at`: a settled server holds tens of thousands of `build_job` rows
//! and the seed read every one of them to keep the handful that are recent.
//! `INCLUDE (derivation)` is what the CTE selects, so the seed is an index-only
//! scan rather than a heap fetch per surviving row.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS \"idx-build_job-created_at\" \
     ON build_job (created_at) INCLUDE (derivation)",
    "CREATE INDEX IF NOT EXISTS \"idx-entry_point-created_at\" \
     ON entry_point (created_at) INCLUDE (derivation)",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-build_job-created_at\"",
    "DROP INDEX IF EXISTS \"idx-entry_point-created_at\"",
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
