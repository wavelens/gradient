/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds `idx-derivation_output-hash-cached`, a partial index on `hash` scoped
//! to `is_cached = true`. The proto `CacheQuery` handler filters
//! `derivation_output` by `hash IN (...) AND is_cached = true` on every worker
//! cache lookup; without this index the planner falls back to a sequential
//! scan that grows with total build outputs ever produced.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE INDEX "idx-derivation_output-hash-cached"
                   ON derivation_output (hash)
                   WHERE is_cached = true"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP INDEX IF EXISTS "idx-derivation_output-hash-cached""#)
            .await?;

        Ok(())
    }
}
