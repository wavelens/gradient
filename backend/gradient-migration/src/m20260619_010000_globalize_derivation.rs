/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Make `derivation` a global, content-addressed graph: drop `organization`,
//! merge duplicate rows by `(hash, name)` onto the surviving (min-id) row, and
//! re-point every foreign key. Irreversible.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

const REPOINTS: &[(&str, &str)] = &[
    ("derivation_output", "derivation"),
    ("derivation_dependency", "derivation"),
    ("derivation_dependency", "dependency"),
    ("derivation_closure", "root_derivation"),
    ("derivation_closure", "dep_derivation"),
    ("derivation_feature", "derivation"),
    ("derivation_metric", "derivation"),
    ("build", "derivation"),
    ("cache_derivation", "derivation"),
    ("acknowledged_derivation", "derivation"),
];

const UNIQUE_PAIRS: &[(&str, &str, &str)] = &[
    ("idx-derivation_output-derivation-name", "derivation_output", "derivation, name"),
    ("idx-derivation_dependency-pair", "derivation_dependency", "derivation, dependency"),
    ("idx-derivation_closure-pair", "derivation_closure", "root_derivation, dep_derivation"),
    ("idx-cache_derivation-pair", "cache_derivation", "cache, derivation"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let b = db.get_database_backend();
        let exec = |sql: String| Statement::from_string(b, sql);

        // Postgres has ordering operators for uuid but no min() aggregate, so
        // first_value over an ordered window yields the surviving (lowest) id.
        db.execute(exec(
            "CREATE TEMP TABLE derivation_dedup AS \
             SELECT id AS old_id, \
                    first_value(id) OVER (PARTITION BY hash, name ORDER BY id) AS keep_id \
             FROM derivation"
                .into(),
        ))
        .await?;

        // Drop the unique indexes that re-pointing would transiently violate.
        for (idx, ..) in UNIQUE_PAIRS {
            db.execute(exec(format!("DROP INDEX IF EXISTS \"{idx}\""))).await?;
        }

        // Re-point every FK to the surviving derivation row.
        for (table, col) in REPOINTS {
            db.execute(exec(format!(
                "UPDATE {table} t SET {col} = d.keep_id \
                 FROM derivation_dedup d \
                 WHERE t.{col} = d.old_id AND d.old_id <> d.keep_id"
            )))
            .await?;
        }

        // Collapse duplicate pairs created by re-pointing, then restore the
        // unique indexes.
        for (idx, table, cols) in UNIQUE_PAIRS {
            let join: String = cols
                .split(',')
                .map(|c| format!("a.{0} = bb.{0}", c.trim()))
                .collect::<Vec<_>>()
                .join(" AND ");
            db.execute(exec(format!(
                "DELETE FROM {table} a USING {table} bb WHERE a.ctid > bb.ctid AND {join}"
            )))
            .await?;
            db.execute(exec(format!(
                "CREATE UNIQUE INDEX \"{idx}\" ON {table} ({cols})"
            )))
            .await?;
        }

        // Drop the now-unreferenced duplicate derivation rows.
        db.execute(exec(
            "DELETE FROM derivation WHERE id IN \
             (SELECT old_id FROM derivation_dedup WHERE old_id <> keep_id)"
                .into(),
        ))
        .await?;

        // Swap the unique index to the global (hash, name) and drop org.
        db.execute(exec("DROP INDEX IF EXISTS \"idx-derivation-org-hash-name\"".into())).await?;
        db.execute(exec(
            "CREATE UNIQUE INDEX \"idx-derivation-hash-name\" ON derivation (hash, name)".into(),
        ))
        .await?;
        db.execute(exec(
            "ALTER TABLE derivation DROP CONSTRAINT IF EXISTS \"fk-derivation-organization\"".into(),
        ))
        .await?;
        db.execute(exec("ALTER TABLE derivation DROP COLUMN IF EXISTS organization".into())).await?;
        db.execute(exec("DROP TABLE IF EXISTS derivation_dedup".into())).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "m20260619_010000_globalize_derivation is irreversible".into(),
        ))
    }
}
