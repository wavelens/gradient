/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One graph, two edge kinds. Runtime references were a second graph at the
//! path level (`cached_path_reference`), so demand could never reach the producer
//! of a missing reference and wholeness was counted on paths while readiness was
//! counted on anchors. Runtime edges now sit on `derivation_dependency` next to
//! the build edges, wholeness is counted on the anchor like `unready_deps`, and the
//! narinfo's ordered `References:` line lives in one text column. The old index
//! stays until every reader has moved; a later migration drops it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE derivation_dependency ADD COLUMN IF NOT EXISTS kind smallint NOT NULL DEFAULT 0",
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS missing_runtime_deps integer NOT NULL DEFAULT 0",
    "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS \"references\" text",
    "INSERT INTO derivation_dependency (derivation, dependency, kind) \
     SELECT DISTINCT o.derivation, ro.derivation, 1 \
     FROM cached_path_reference r \
     JOIN derivation_output o ON o.hash = r.referrer \
     JOIN derivation_output ro ON ro.hash = r.reference_hash \
     WHERE o.derivation <> ro.derivation \
     ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0",
    "UPDATE cached_path cp SET \"references\" = x.refs \
     FROM (SELECT r.referrer, string_agg(r.reference, ' ' ORDER BY r.position) AS refs \
           FROM cached_path_reference r GROUP BY r.referrer) x \
     WHERE cp.hash = x.referrer",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_dependency-runtime\" \
     ON derivation_dependency (dependency) INCLUDE (derivation) WHERE kind IN (1, 2)",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_dependency-runtime\"",
    "ALTER TABLE cached_path DROP COLUMN IF EXISTS \"references\"",
    "ALTER TABLE derivation_build DROP COLUMN IF EXISTS missing_runtime_deps",
    "DELETE FROM derivation_dependency WHERE kind = 1",
    "ALTER TABLE derivation_dependency DROP COLUMN IF EXISTS kind",
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
    use super::UP;

    /// The backfill joins the old index through derivation_output twice: the
    /// referrer's producer and the reference's producer. An edge that already
    /// exists as a build edge becomes Both, never a duplicate row.
    #[test]
    fn the_runtime_edge_backfill_merges_into_existing_build_edges() {
        let backfill = UP
            .iter()
            .find(|s| s.contains("INSERT INTO derivation_dependency"))
            .expect("the backfill statement");
        assert!(
            backfill.contains("JOIN derivation_output ro ON ro.hash = r.reference_hash"),
            "{backfill}"
        );
        assert!(
            backfill.contains("JOIN derivation_output o ON o.hash = r.referrer"),
            "{backfill}"
        );
        assert!(
            backfill.contains(
                "ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0"
            ),
            "{backfill}"
        );
        assert!(
            backfill.contains("WHERE o.derivation <> ro.derivation"),
            "a self reference is not an edge: {backfill}"
        );
    }

    #[test]
    fn the_references_column_keeps_the_narinfo_order() {
        let refs = UP
            .iter()
            .find(|s| s.contains("SET \"references\" ="))
            .expect("the references backfill");
        assert!(
            refs.contains("string_agg(r.reference, ' ' ORDER BY r.position)"),
            "{refs}"
        );
    }
}
