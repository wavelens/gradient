/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.derivation_output
    ALTER COLUMN hash DROP NOT NULL,
    ALTER COLUMN package DROP NOT NULL,
    ADD COLUMN path_known_after_build boolean DEFAULT false NOT NULL;
UPDATE public.derivation_output
SET hash = NULL, package = NULL, path_known_after_build = true
WHERE hash = 'unknown';
"#;

const DOWN: &str = r#"
UPDATE public.derivation_output
SET hash = 'unknown', package = name
WHERE hash IS NULL;
ALTER TABLE public.derivation_output
    ALTER COLUMN hash SET NOT NULL,
    ALTER COLUMN package SET NOT NULL,
    DROP COLUMN path_known_after_build;
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
