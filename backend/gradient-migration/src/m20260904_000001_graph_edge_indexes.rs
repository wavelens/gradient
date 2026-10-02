/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// `ADD PRIMARY KEY USING INDEX` is adopting the natural pair index without a rebuild. It is also
/// renaming each `idx-*-pair` index to the table's `_pkey` name.
const STATEMENTS: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_dependency-reverse-pair\" \
     ON derivation_dependency (dependency, derivation)",
    "DROP INDEX IF EXISTS \"idx-derivation_dependency-dependency\"",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path_reference-referrer-hash\" \
     ON cached_path_reference (referrer, reference_hash)",
    "ALTER TABLE derivation_dependency DROP CONSTRAINT IF EXISTS derivation_dependency_pkey",
    "ALTER TABLE derivation_dependency \
     ADD PRIMARY KEY USING INDEX \"idx-derivation_dependency-pair\"",
    "ALTER TABLE derivation_dependency DROP COLUMN IF EXISTS id",
    "ALTER TABLE derivation_closure DROP CONSTRAINT IF EXISTS derivation_closure_pkey",
    "ALTER TABLE derivation_closure ADD PRIMARY KEY USING INDEX \"idx-derivation_closure-pair\"",
    "ALTER TABLE derivation_closure DROP COLUMN IF EXISTS id",
    "ALTER TABLE cached_path_reference DROP CONSTRAINT IF EXISTS cached_path_reference_pkey",
    "ALTER TABLE cached_path_reference \
     ADD PRIMARY KEY USING INDEX \"idx-cached_path_reference-pair\"",
    "ALTER TABLE cached_path_reference DROP COLUMN IF EXISTS id",
];

/// The backfills are rewriting every row. This reverse migration is far more expensive than the
/// forward one.
const REVERT: &[&str] = &[
    "ALTER TABLE cached_path_reference DROP CONSTRAINT IF EXISTS cached_path_reference_pkey",
    "ALTER TABLE cached_path_reference ADD COLUMN IF NOT EXISTS id uuid",
    "UPDATE cached_path_reference SET id = uuidv7() WHERE id IS NULL",
    "ALTER TABLE cached_path_reference ALTER COLUMN id SET NOT NULL",
    "ALTER TABLE cached_path_reference ADD PRIMARY KEY (id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS \"idx-cached_path_reference-pair\" \
     ON cached_path_reference (referrer, reference)",
    "DROP INDEX IF EXISTS \"idx-cached_path_reference-referrer-hash\"",
    "ALTER TABLE derivation_closure DROP CONSTRAINT IF EXISTS derivation_closure_pkey",
    "ALTER TABLE derivation_closure ADD COLUMN IF NOT EXISTS id uuid",
    "UPDATE derivation_closure SET id = uuidv7() WHERE id IS NULL",
    "ALTER TABLE derivation_closure ALTER COLUMN id SET NOT NULL",
    "ALTER TABLE derivation_closure ADD PRIMARY KEY (id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS \"idx-derivation_closure-pair\" \
     ON derivation_closure (root_derivation, dep_derivation)",
    "ALTER TABLE derivation_dependency DROP CONSTRAINT IF EXISTS derivation_dependency_pkey",
    "ALTER TABLE derivation_dependency ADD COLUMN IF NOT EXISTS id uuid",
    "UPDATE derivation_dependency SET id = uuidv7() WHERE id IS NULL",
    "ALTER TABLE derivation_dependency ALTER COLUMN id SET NOT NULL",
    "ALTER TABLE derivation_dependency ADD PRIMARY KEY (id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS \"idx-derivation_dependency-pair\" \
     ON derivation_dependency (derivation, dependency)",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_dependency-dependency\" \
     ON derivation_dependency (dependency)",
    "DROP INDEX IF EXISTS \"idx-derivation_dependency-reverse-pair\"",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in STATEMENTS {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in REVERT {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
