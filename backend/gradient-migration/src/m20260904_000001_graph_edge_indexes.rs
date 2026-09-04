/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Covering indexes for the two reverse graph walks, and retirement of the
/// surrogate primary keys on the three junction tables.
///
/// The walks project the far end of an edge, so an index on the probed column
/// alone forces a heap fetch per row: the dependents walk spent 871k buffers on
/// 68k nodes, and the `cached_path_reference` walk (57% of all database time)
/// bitmap-scanned 82,925 heap blocks per pass. Adding the projected column makes
/// both index-only.
///
/// The surrogate `id` keys go because none of the three has ever been read:
/// `idx_scan = 0` over an eleven-day production window on `derivation_closure`
/// (909 MB), `cached_path_reference` (372 MB) and `derivation_dependency`
/// (161 MB). Each one is a third of the index maintenance on the hottest insert
/// paths in the system. The natural pair already carries a unique index, so
/// `ADD PRIMARY KEY USING INDEX` adopts it without a rebuild; note that this
/// renames each `idx-*-pair` index to the table's `_pkey` name.
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

/// Restores the surrogate keys. The backfills rewrite every row, so this is far
/// more expensive than the forward migration.
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
