/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Normalize `cached_path.references` (a space-separated `hash-name` text blob)
//! into its own relation. The self-heal paths found referrers with `references
//! LIKE '%hash%'`, a full table scan per call; the indexed `reference_hash`
//! column makes those exact lookups. `position` records each reference's index in
//! the order the worker sent it (nix `StorePathSet` / store-path order), so the
//! narinfo `References:` line and the signature fingerprint reconstruct verbatim
//! via `ORDER BY position` - no dependency on Postgres collation. Backfilled from
//! the existing column before it is dropped.

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
            CREATE TABLE IF NOT EXISTS cached_path_reference (
                id UUID PRIMARY KEY,
                referrer TEXT NOT NULL REFERENCES cached_path (hash) ON DELETE CASCADE,
                reference TEXT NOT NULL,
                reference_hash TEXT NOT NULL,
                position INTEGER NOT NULL
            )
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx-cached_path_reference-pair"
               ON cached_path_reference (referrer, reference)"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS "idx-cached_path_reference-order"
               ON cached_path_reference (referrer, position)"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS "idx-cached_path_reference-reference_hash"
               ON cached_path_reference (reference_hash)"#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            INSERT INTO cached_path_reference (id, referrer, reference, reference_hash, position)
            SELECT uuidv7(), cp.hash, t.tok, split_part(t.tok, '-', 1), t.ord
            FROM cached_path cp
            CROSS JOIN LATERAL regexp_split_to_table(coalesce(cp."references", ''), E'\\s+')
                WITH ORDINALITY AS t(tok, ord)
            WHERE t.tok <> ''
            ON CONFLICT (referrer, reference) DO NOTHING
            "#,
        )
        .await?;

        db.execute_unprepared(r#"ALTER TABLE cached_path DROP COLUMN IF EXISTS "references""#)
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(r#"ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS "references" TEXT"#)
            .await?;

        db.execute_unprepared(
            r#"
            UPDATE cached_path cp SET "references" = sub.refs
            FROM (
                SELECT referrer, string_agg(reference, ' ' ORDER BY position) AS refs
                FROM cached_path_reference GROUP BY referrer
            ) sub
            WHERE cp.hash = sub.referrer
            "#,
        )
        .await?;

        db.execute_unprepared("DROP TABLE IF EXISTS cached_path_reference")
            .await?;

        Ok(())
    }
}
