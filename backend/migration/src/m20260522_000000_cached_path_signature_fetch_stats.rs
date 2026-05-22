/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Add per-(cache, path) fetch tracking columns to `cached_path_signature`,
//! plus indexes that support listing and recency-sorted queries.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
            ALTER TABLE cached_path_signature
              ADD COLUMN last_fetched_at TIMESTAMP NULL,
              ADD COLUMN fetch_count BIGINT NOT NULL DEFAULT 0
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE INDEX "idx-cached_path_signature-cache"
              ON cached_path_signature (cache)
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE INDEX "idx-cached_path_signature-cache-last_fetched_at"
              ON cached_path_signature (cache, last_fetched_at DESC NULLS LAST)
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            r#"DROP INDEX IF EXISTS "idx-cached_path_signature-cache-last_fetched_at""#,
        )
        .await?;
        conn.execute_unprepared(r#"DROP INDEX IF EXISTS "idx-cached_path_signature-cache""#)
            .await?;
        conn.execute_unprepared(
            r#"
            ALTER TABLE cached_path_signature
              DROP COLUMN fetch_count,
              DROP COLUMN last_fetched_at
            "#,
        )
        .await?;
        Ok(())
    }
}
