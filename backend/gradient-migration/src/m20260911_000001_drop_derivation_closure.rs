/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE evaluation ADD COLUMN IF NOT EXISTS graph_version bigint NOT NULL DEFAULT 0",
    "ALTER TABLE entry_point ADD COLUMN IF NOT EXISTS dep_counts_version bigint",
    "ALTER TABLE entry_point ADD COLUMN IF NOT EXISTS dep_counts_computed_at timestamp",
    r#"CREATE INDEX IF NOT EXISTS "idx-entry_point-evaluation-eval"
       ON entry_point (evaluation, eval, id)"#,
    "DROP TABLE IF EXISTS derivation_closure",
    "ALTER TABLE derivation DROP COLUMN IF EXISTS dep_closure_count",
];

const DOWN: &[&str] = &[
    "ALTER TABLE derivation ADD COLUMN IF NOT EXISTS dep_closure_count bigint",
    "CREATE TABLE IF NOT EXISTS derivation_closure ( \
       root_derivation uuid NOT NULL REFERENCES derivation(id) ON DELETE CASCADE, \
       dep_derivation uuid NOT NULL REFERENCES derivation(id) ON DELETE CASCADE, \
       PRIMARY KEY (root_derivation, dep_derivation))",
    r#"DROP INDEX IF EXISTS "idx-entry_point-evaluation-eval""#,
    "ALTER TABLE entry_point DROP COLUMN IF EXISTS dep_counts_computed_at",
    "ALTER TABLE entry_point DROP COLUMN IF EXISTS dep_counts_version",
    "ALTER TABLE evaluation DROP COLUMN IF EXISTS graph_version",
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
