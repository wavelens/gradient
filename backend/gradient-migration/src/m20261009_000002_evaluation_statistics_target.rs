/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A default statistics target is listing every evaluation as a most common value, and an
//! evaluation newer than the last ANALYZE is then estimated at a single row. A short list is
//! estimating it from the evaluations outside the list instead.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE build_job ALTER COLUMN evaluation SET STATISTICS 10",
    "ALTER TABLE entry_point ALTER COLUMN evaluation SET STATISTICS 10",
    "ANALYZE build_job",
    "ANALYZE entry_point",
];

const DOWN: &[&str] = &[
    "ALTER TABLE build_job ALTER COLUMN evaluation SET STATISTICS -1",
    "ALTER TABLE entry_point ALTER COLUMN evaluation SET STATISTICS -1",
    "ANALYZE build_job",
    "ANALYZE entry_point",
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
