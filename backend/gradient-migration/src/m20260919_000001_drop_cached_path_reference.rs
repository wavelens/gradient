/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The path-level reference graph goes. Runtime references are edges of
//! `derivation_dependency` now and wholeness is counted on the anchor
//! (`derivation_build.missing_runtime_deps`), so `cached_path_reference` and the
//! counter it fed have no readers left: `m20260919_000000` copied the edges onto
//! the graph and the ordered `References:` line into `cached_path.references`.
//!
//! `down` restores the table, its indexes and the column exactly as
//! `m20260624_000003` and `m20260908_000001` created them, but NOT the rows: the
//! edges are recoverable from `cached_path.references` and are re-derived here,
//! while the counter is left at its default for the same reason the forward
//! migration carries no backfill - a recount is what fills it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "DROP TABLE IF EXISTS cached_path_reference",
    "DROP INDEX IF EXISTS \"idx-cached_path-negative_references\"",
    "ALTER TABLE cached_path DROP COLUMN IF EXISTS missing_references",
];

const DOWN: &[&str] = &[
    "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS missing_references integer NOT NULL DEFAULT 0",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path-negative_references\" \
     ON cached_path (hash) WHERE missing_references < 0",
    "CREATE TABLE IF NOT EXISTS cached_path_reference ( \
         referrer TEXT NOT NULL REFERENCES cached_path (hash) ON DELETE CASCADE, \
         reference TEXT NOT NULL, \
         reference_hash TEXT NOT NULL, \
         position INTEGER NOT NULL, \
         PRIMARY KEY (referrer, reference))",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path_reference-order\" \
     ON cached_path_reference (referrer, position)",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path_reference-reference_hash\" \
     ON cached_path_reference (reference_hash)",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path_reference-referrer-hash\" \
     ON cached_path_reference (referrer, reference_hash)",
    "INSERT INTO cached_path_reference (referrer, reference, reference_hash, position) \
     SELECT cp.hash, t.tok, split_part(t.tok, '-', 1), t.ord \
     FROM cached_path cp \
     CROSS JOIN LATERAL regexp_split_to_table(coalesce(cp.\"references\", ''), E'\\\\s+') \
         WITH ORDINALITY AS t(tok, ord) \
     WHERE t.tok <> '' \
     ON CONFLICT (referrer, reference) DO NOTHING",
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

#[cfg(test)]
mod tests {
    use super::{DOWN, UP};

    /// The table goes only after its column does, or the reverse migration has
    /// nothing to rebuild the edges from.
    #[test]
    fn the_reverse_rebuilds_the_index_from_the_column_that_replaced_it() {
        assert!(
            UP.iter()
                .any(|s| s.contains("DROP TABLE IF EXISTS cached_path_reference"))
        );
        assert!(
            UP.iter().all(
                |s| !s.contains("ALTER TABLE cached_path DROP COLUMN IF EXISTS \"references\"")
            ),
            "the narinfo line stays: it is what the reverse reads"
        );
        let backfill = DOWN
            .iter()
            .find(|s| s.contains("INSERT INTO cached_path_reference"))
            .expect("the reverse backfill");
        assert!(backfill.contains("cp.\"references\""), "{backfill}");
        assert!(
            backfill.contains("WITH ORDINALITY AS t(tok, ord)"),
            "{backfill}"
        );
    }
}
