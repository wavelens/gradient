/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const BROKEN: &str = r#"
    WITH RECURSIVE broken(hash) AS (
            SELECT cp.hash FROM cached_path cp WHERE cp.file_hash IS NULL
          UNION
            SELECT r.reference_hash FROM cached_path_reference r
            LEFT JOIN cached_path dep ON dep.hash = r.reference_hash
            WHERE dep.hash IS NULL
          UNION
            SELECT s.next FROM broken c, LATERAL (
                SELECT r.referrer AS next FROM cached_path_reference r
                WHERE r.reference_hash = c.hash AND r.referrer <> c.hash OFFSET 0) s)
"#;

fn seed() -> String {
    format!(
        "{BROKEN} \
         UPDATE cached_path cp SET missing_references = m.n \
         FROM (SELECT r.referrer AS hash, count(*) AS n FROM cached_path_reference r \
               JOIN broken b ON b.hash = r.reference_hash \
               WHERE r.referrer <> r.reference_hash GROUP BY r.referrer) m \
         WHERE cp.hash = m.hash"
    )
}

fn up_statements() -> Vec<String> {
    vec![
        "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS missing_references integer NOT NULL DEFAULT 0".into(),
        "UPDATE cached_path SET missing_references = 0 WHERE missing_references <> 0".into(),
        seed(),
        "DROP INDEX IF EXISTS \"idx-cached_path-closure_complete\"".into(),
        "DROP INDEX IF EXISTS \"idx-cached_path-closure_pending\"".into(),
        "ALTER TABLE cached_path DROP COLUMN IF EXISTS closure_complete".into(),
        "CREATE INDEX IF NOT EXISTS \"idx-cached_path-negative_references\" ON cached_path (hash) WHERE missing_references < 0".into(),
    ]
}

const DOWN: &[&str] = &[
    "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS closure_complete boolean NOT NULL DEFAULT false",
    "UPDATE cached_path SET closure_complete = (file_hash IS NOT NULL AND missing_references = 0)",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path-closure_complete\" ON cached_path (hash) WHERE closure_complete",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path-closure_pending\" ON cached_path (hash) WHERE NOT closure_complete AND file_hash IS NOT NULL",
    "DROP INDEX IF EXISTS \"idx-cached_path-negative_references\"",
    "ALTER TABLE cached_path DROP COLUMN IF EXISTS missing_references",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in up_statements() {
            manager.get_connection().execute_unprepared(&stmt).await?;
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
    use super::{DOWN, seed, up_statements};

    #[test]
    fn the_backfill_never_reads_the_flag_it_replaces() {
        let drop_column = "ALTER TABLE cached_path DROP COLUMN IF EXISTS closure_complete";
        for stmt in up_statements() {
            assert!(
                !stmt.contains("closure_complete")
                    || stmt.starts_with("DROP INDEX IF EXISTS")
                    || stmt == drop_column,
                "up reads closure_complete: {stmt}"
            );
        }

        assert!(
            up_statements().iter().any(|s| s == drop_column),
            "up must drop closure_complete once nothing reads it"
        );
        assert!(
            DOWN.iter().any(|s| s.contains("closure_complete")),
            "down must restore the flag it dropped"
        );
    }

    /// The recursive term must stay fenced with `LATERAL (... OFFSET 0)`. The planner is
    /// merge-joining the edge table once per iteration without the fence.
    #[test]
    fn the_seed_is_one_fenced_walk() {
        let sql = seed().split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(!sql.contains("LOOP"), "the seed must not iterate: {sql}");
        assert!(sql.contains("WITH RECURSIVE broken(hash) AS ("), "{sql}");
        assert!(
            sql.contains("FROM broken c, LATERAL ( SELECT r.referrer AS next"),
            "the recursive term walks referrer-ward: {sql}"
        );
        assert!(
            sql.contains("AND r.referrer <> c.hash OFFSET 0) s)"),
            "the recursive term must stay fenced and skip self-references: {sql}"
        );
    }

    #[test]
    fn the_walk_seeds_from_the_unbacked_and_the_absent() {
        let sql = seed().split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(
            sql.contains("SELECT cp.hash FROM cached_path cp WHERE cp.file_hash IS NULL"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "LEFT JOIN cached_path dep ON dep.hash = r.reference_hash WHERE dep.hash IS NULL"
            ),
            "{sql}"
        );
    }

    /// A self-reference is not an edge into the broken set. The live recount in `gradient_db` is
    /// excluding `reference_hash = hash`, and the seed must agree with it.
    #[test]
    fn the_count_excludes_self_references() {
        let sql = seed().split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(
            sql.contains(
                "JOIN broken b ON b.hash = r.reference_hash \
                 WHERE r.referrer <> r.reference_hash GROUP BY r.referrer"
            ),
            "{sql}"
        );
    }

    #[test]
    fn the_seed_is_preceded_by_a_reset() {
        let stmts = up_statements();
        let reset = stmts
            .iter()
            .position(|s| {
                s == "UPDATE cached_path SET missing_references = 0 WHERE missing_references <> 0"
            })
            .expect("the reset runs");
        let seeded = stmts
            .iter()
            .position(|s| s.contains("WITH RECURSIVE broken"))
            .expect("the seed runs");

        assert!(reset < seeded, "the reset must precede the seed");
        assert!(
            stmts[0].contains("ADD COLUMN IF NOT EXISTS missing_references"),
            "the column exists before either: {:?}",
            stmts[0]
        );
    }
}
