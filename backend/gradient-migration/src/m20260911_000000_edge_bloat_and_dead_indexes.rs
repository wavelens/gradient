/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

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

    /// The natural key must exist before its backing unique constraint is dropped. A duplicate
    /// `(derivation, hash)` in between would block the new primary key forever.
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

    /// Only the redundant non-unique copy is dropped. `cached_path_hash_key` is backing the
    /// uniqueness that narinfo lookups and every `ON CONFLICT (hash)` upsert rely on.
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
