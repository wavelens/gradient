/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds the DWARF build-id index backing `GET /cache/{cache}/debuginfo/{build_id}`.
//! Each row maps a 40-hex build id to the `cached_path` whose NAR carries the
//! `lib/debug/.build-id/<xx>/<yy>.debug` member, mirroring what nix writes when a
//! binary cache is created with `index-debug-info=true`.
//! `cached_path.debug_info_indexed` marks a NAR as already scanned so the
//! backfill sweep never re-reads it - including the common case of no build ids.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS debug_info (
                id UUID PRIMARY KEY,
                build_id TEXT NOT NULL,
                cached_path UUID NOT NULL REFERENCES cached_path (id) ON DELETE CASCADE,
                member TEXT NOT NULL,
                created_at TIMESTAMP NOT NULL
            )
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx-debug_info-pair"
               ON debug_info (build_id, cached_path)"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS "idx-debug_info-build_id"
               ON debug_info (build_id)"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS "idx-debug_info-cached_path"
               ON debug_info (cached_path)"#,
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE cached_path \
             ADD COLUMN IF NOT EXISTS debug_info_indexed boolean NOT NULL DEFAULT false",
        )
        .await?;

        // Partial index over exactly the sweep's predicate: separate-debug-info
        // outputs are a small slice of the cache, so the backfill never scans the
        // full cached_path table.
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS "idx-cached_path-debug_info_pending"
               ON cached_path (created_at)
               WHERE NOT debug_info_indexed AND package LIKE '%-debug'"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(r#"DROP INDEX IF EXISTS "idx-cached_path-debug_info_pending""#)
            .await?;
        db.execute_unprepared("ALTER TABLE cached_path DROP COLUMN IF EXISTS debug_info_indexed")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS debug_info")
            .await?;
        Ok(())
    }
}
