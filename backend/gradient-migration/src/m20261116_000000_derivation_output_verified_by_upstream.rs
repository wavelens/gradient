/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.derivation_output ADD COLUMN verified_by_upstream uuid,
    ADD CONSTRAINT "fk-derivation_output-verified_by_upstream" FOREIGN KEY (verified_by_upstream) REFERENCES public.cache_upstream(id) ON DELETE SET NULL;

CREATE INDEX "idx-derivation_output-verified_by_upstream" ON public.derivation_output USING btree (verified_by_upstream) WHERE (verified_by_upstream IS NOT NULL);
"#;

const DOWN: &str = r#"
ALTER TABLE public.derivation_output DROP COLUMN verified_by_upstream;
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
