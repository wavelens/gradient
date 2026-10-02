/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Deleting a parent without an index on the referencing column is a sequential scan per deleted
//! row. Only keys under parents the GC is deleting in bulk are indexed here.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_output-cached_path"
     ON derivation_output (cached_path) WHERE cached_path IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_metric-derivation"
     ON derivation_metric (derivation)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-cache_derivation-derivation"
     ON cache_derivation (derivation)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-evaluation-previous"
     ON evaluation (previous) WHERE previous IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-evaluation-next"
     ON evaluation (next) WHERE next IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-entry_point_message-message"
     ON entry_point_message (message)"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-entry_point_message-message""#,
    r#"DROP INDEX IF EXISTS "idx-evaluation-next""#,
    r#"DROP INDEX IF EXISTS "idx-evaluation-previous""#,
    r#"DROP INDEX IF EXISTS "idx-cache_derivation-derivation""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_metric-derivation""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_output-cached_path""#,
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
