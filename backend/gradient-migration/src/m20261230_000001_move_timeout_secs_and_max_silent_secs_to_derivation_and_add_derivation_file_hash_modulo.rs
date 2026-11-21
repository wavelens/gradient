/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Build limits move from `derivation_build` to `derivation`. The `derivation` table can store the
// hash modulo of its own `.drv` file. This value is NULL until a verified record can fill it.
// Writes to `derivation` take only a verified `.drv` file as input. The timeout_secs and
// max_silent_secs columns of derivation_build stay during the rolling upgrade for servers that
// were built from main before this change. They go away in the next release.
const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.derivation
    ADD COLUMN timeout_secs bigint,
    ADD COLUMN max_silent_secs bigint,
    ADD COLUMN serialized_derivation_hash_for_dependent_output_paths text;

UPDATE public.derivation AS d
SET timeout_secs = db.timeout_secs, max_silent_secs = db.max_silent_secs
FROM public.derivation_build AS db
WHERE db.derivation = d.id
  AND (db.timeout_secs IS NOT NULL OR db.max_silent_secs IS NOT NULL);
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
UPDATE public.derivation_build AS db
SET timeout_secs = d.timeout_secs, max_silent_secs = d.max_silent_secs
FROM public.derivation AS d
WHERE db.derivation = d.id
  AND (d.timeout_secs IS NOT NULL OR d.max_silent_secs IS NOT NULL);

ALTER TABLE public.derivation
    DROP COLUMN timeout_secs,
    DROP COLUMN max_silent_secs,
    DROP COLUMN serialized_derivation_hash_for_dependent_output_paths;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
