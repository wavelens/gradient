/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-table autovacuum for the three edge tables, retirement of two dead
//! indexes, and an extended statistics object over the readiness columns.
//!
//! The edge tables are append-heavy and read through index-only scans, so what
//! keeps them fast is the visibility map, not the dead-tuple count. At the
//! cluster defaults (`vacuum`/`analyze` 0.2, insert 0.2) `derivation_dependency`
//! was vacuumed twice in seventeen days and carried 522k dead of 3.8M tuples,
//! `derivation_closure` 2.1M dead of 20.1M, and their pair indexes fetched 22%
//! and 15% of the tuples they read back from the heap. `cached_path_reference`
//! shows the other half of the same problem: 58 dead tuples and 440k
//! modifications, so no dead-tuple threshold will ever fire on it and only the
//! insert threshold can set its visibility map. All three scale factors go to
//! 0.02 on all three tables.
//!
//! Both dropped indexes are redundant rather than merely cold.
//! `idx-cached_path-hash` is a non-unique btree on `cached_path (hash)` that
//! duplicates the unique constraint index `cached_path_hash_key` on the same
//! column; the planner can serve every one of its 29 billion scans from the
//! unique index, and dropping it takes 50 MB and one index write off a table
//! with 2.8M updates. `derivation_input_source_pkey` has never been scanned:
//! nothing selects a source row by its surrogate id and nothing has a foreign
//! key onto it, so the natural key `(derivation, hash)` becomes the primary key
//! and the `id` column goes with its index.
//!
//! `idx-cached_path_reference-order` is rebuilt as a covering index. It is
//! `(referrer, "position")`, and its only reader is
//! `runtime_closure::references_for_hash`, which projects `reference` to
//! rebuild a narinfo `References:` line: 109 billion index tuples read and
//! 108.6 billion of them fetched back from the heap, because the column it
//! selects is the one column the index does not carry. `INCLUDE (reference)`
//! makes that scan index-only and costs roughly the index's own size again.
//!
//! `idx-cached_path_reference-pair` is deliberately left alone despite the same
//! 97.6% ratio. It is the UNIQUE `(referrer, reference)` the reference upsert
//! names as its conflict target, so the heap access IS the row being updated and
//! no payload column can remove it; it reads 30 million tuples against the order
//! index's 109 billion.
//!
//! The statistics object teaches the planner that `status`, `fetchable` and
//! `unready_deps` are correlated. Every dispatch and promotion predicate filters
//! on two or three of them at once, and independent selectivity underestimates
//! those conjunctions by the product of three fractions.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// The edge tables whose visibility map has to stay fresh for the graph walks
/// to keep their index-only scans.
const EDGE_TABLES: [&str; 3] = [
    "cached_path_reference",
    "derivation_dependency",
    "derivation_closure",
];

const AUTOVACUUM_SETTINGS: &str = "autovacuum_vacuum_scale_factor = 0.02, \
     autovacuum_analyze_scale_factor = 0.02, \
     autovacuum_vacuum_insert_scale_factor = 0.02";

const AUTOVACUUM_DEFAULTS: [&str; 3] = [
    "autovacuum_vacuum_scale_factor",
    "autovacuum_analyze_scale_factor",
    "autovacuum_vacuum_insert_scale_factor",
];

fn up_statements() -> Vec<String> {
    let mut stmts: Vec<String> = EDGE_TABLES
        .iter()
        .map(|t| format!("ALTER TABLE {t} SET ({AUTOVACUUM_SETTINGS})"))
        .collect();

    stmts.extend(
        [
            r#"DROP INDEX IF EXISTS "idx-cached_path-hash""#,
            r#"DROP INDEX IF EXISTS "idx-cached_path_reference-order""#,
            r#"CREATE INDEX IF NOT EXISTS "idx-cached_path_reference-order"
               ON cached_path_reference (referrer, "position") INCLUDE (reference)"#,
            "ALTER TABLE derivation_input_source DROP CONSTRAINT IF EXISTS \
             derivation_input_source_pkey",
            "ALTER TABLE derivation_input_source ADD PRIMARY KEY (derivation, hash)",
            "ALTER TABLE derivation_input_source DROP CONSTRAINT IF EXISTS \
             derivation_input_source_derivation_hash_key",
            "ALTER TABLE derivation_input_source DROP COLUMN IF EXISTS id",
            r#"CREATE STATISTICS IF NOT EXISTS "stat-derivation_build-readiness"
               ON status, fetchable, unready_deps FROM derivation_build"#,
            "ANALYZE derivation_build",
        ]
        .map(str::to_owned),
    );

    stmts
}

fn down_statements() -> Vec<String> {
    let mut stmts = vec![
        r#"DROP STATISTICS IF EXISTS "stat-derivation_build-readiness""#.to_owned(),
        "ALTER TABLE derivation_input_source ADD CONSTRAINT \
         derivation_input_source_derivation_hash_key UNIQUE (derivation, hash)"
            .to_owned(),
        "ALTER TABLE derivation_input_source DROP CONSTRAINT IF EXISTS \
         derivation_input_source_pkey"
            .to_owned(),
        "ALTER TABLE derivation_input_source ADD COLUMN IF NOT EXISTS id uuid".to_owned(),
        "UPDATE derivation_input_source SET id = uuidv7() WHERE id IS NULL".to_owned(),
        "ALTER TABLE derivation_input_source ALTER COLUMN id SET NOT NULL".to_owned(),
        "ALTER TABLE derivation_input_source ADD PRIMARY KEY (id)".to_owned(),
        r#"CREATE INDEX IF NOT EXISTS "idx-cached_path-hash" ON cached_path (hash)"#.to_owned(),
        r#"DROP INDEX IF EXISTS "idx-cached_path_reference-order""#.to_owned(),
        r#"CREATE INDEX IF NOT EXISTS "idx-cached_path_reference-order"
           ON cached_path_reference (referrer, "position")"#
            .to_owned(),
    ];

    stmts.extend(
        EDGE_TABLES
            .iter()
            .map(|t| format!("ALTER TABLE {t} RESET ({})", AUTOVACUUM_DEFAULTS.join(", "))),
    );

    stmts
}

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
        for stmt in down_statements() {
            manager.get_connection().execute_unprepared(&stmt).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AUTOVACUUM_DEFAULTS, EDGE_TABLES, down_statements, up_statements};

    /// The narinfo reference read projects the one column the order index does
    /// not carry, so the rebuild has to add it as a payload and has to drop the
    /// old definition first, or `IF NOT EXISTS` silently keeps the uncovered one.
    #[test]
    fn the_order_index_covers_the_column_its_only_reader_projects() {
        let up = up_statements();
        let drop = up
            .iter()
            .position(|s| s.contains(r#"DROP INDEX IF EXISTS "idx-cached_path_reference-order""#))
            .expect("the old definition goes");
        let create = up
            .iter()
            .position(|s| {
                s.contains(r#"CREATE INDEX IF NOT EXISTS "idx-cached_path_reference-order""#)
            })
            .expect("the covering definition lands");

        assert!(drop < create, "{up:?}");
        assert!(up[create].contains("INCLUDE (reference)"), "{}", up[create]);
        assert!(
            up[create].contains(r#"(referrer, "position")"#),
            "the key columns must not change: {}",
            up[create]
        );
        assert!(
            !up.iter()
                .any(|s| s.contains("idx-cached_path_reference-pair")),
            "the pair index is the upsert's conflict target, so a payload cannot \
             remove its heap access: {up:?}"
        );
    }

    /// A dead-tuple threshold alone never fires on an append-only edge table
    /// (`cached_path_reference` carries 58 dead tuples against 7.7M live), so the
    /// insert scale factor is the one that keeps its visibility map set and its
    /// pair-index scans index-only.
    #[test]
    fn every_edge_table_gets_all_three_scale_factors() {
        for table in EDGE_TABLES {
            let stmt = up_statements()
                .into_iter()
                .find(|s| s.starts_with(&format!("ALTER TABLE {table} SET (")))
                .unwrap_or_else(|| panic!("{table} is tuned"));

            for setting in AUTOVACUUM_DEFAULTS {
                assert!(stmt.contains(&format!("{setting} = 0.02")), "{stmt}");
            }
        }
    }

    /// The natural key has to be in place before the unique constraint backing it
    /// is dropped, or the window between the two statements accepts a duplicate
    /// `(derivation, hash)` that the new primary key can then never be built over.
    #[test]
    fn the_natural_key_lands_before_the_unique_constraint_goes() {
        let stmts = up_statements();
        let pos = |needle: &str| {
            stmts
                .iter()
                .position(|s| s.contains(needle))
                .unwrap_or_else(|| panic!("missing: {needle}"))
        };

        assert!(
            pos("ADD PRIMARY KEY (derivation, hash)")
                < pos("DROP CONSTRAINT IF EXISTS derivation_input_source_derivation_hash_key"),
            "{stmts:?}"
        );
        assert!(
            pos("DROP CONSTRAINT IF EXISTS derivation_input_source_derivation_hash_key")
                < pos("DROP COLUMN IF EXISTS id"),
            "{stmts:?}"
        );
    }

    /// Only the redundant non-unique copy goes. Dropping `cached_path_hash_key`
    /// would drop the uniqueness of `cached_path.hash`, which the narinfo lookups
    /// and every `ON CONFLICT (hash)` upsert rely on.
    #[test]
    fn the_unique_index_on_cached_path_hash_survives() {
        for stmt in up_statements() {
            assert!(!stmt.contains("cached_path_hash_key"), "{stmt}");
        }

        assert!(
            up_statements()
                .iter()
                .any(|s| s == r#"DROP INDEX IF EXISTS "idx-cached_path-hash""#),
            "the duplicate copy goes"
        );
    }

    /// The extended statistics object has to be analyzed before the planner can
    /// read it, and `down` has to remove it or a re-run of `up` finds it there.
    #[test]
    fn the_readiness_statistics_are_created_analyzed_and_reversible() {
        let stmts = up_statements();
        let created = stmts
            .iter()
            .position(|s| s.contains("CREATE STATISTICS"))
            .expect("statistics are created");

        assert!(
            stmts[created].contains("ON status, fetchable, unready_deps"),
            "{stmts:?}"
        );
        assert!(
            stmts
                .iter()
                .skip(created)
                .any(|s| s == "ANALYZE derivation_build"),
            "{stmts:?}"
        );
        assert!(
            down_statements()
                .iter()
                .any(|s| s.contains("DROP STATISTICS")),
            "{:?}",
            down_statements()
        );
    }

    /// `down` restores the surrogate key and both index sets, so the pair is a
    /// round trip rather than a one-way tuning change.
    #[test]
    fn down_restores_the_surrogate_key_and_the_defaults() {
        let stmts = down_statements();
        for needle in [
            "ADD COLUMN IF NOT EXISTS id uuid",
            "ADD PRIMARY KEY (id)",
            r#"CREATE INDEX IF NOT EXISTS "idx-cached_path-hash""#,
        ] {
            assert!(
                stmts.iter().any(|s| s.contains(needle)),
                "missing: {needle}"
            );
        }

        for table in EDGE_TABLES {
            assert!(
                stmts
                    .iter()
                    .any(|s| s.starts_with(&format!("ALTER TABLE {table} RESET ("))),
                "{table} keeps its overrides"
            );
        }
    }
}
