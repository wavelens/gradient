/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
DROP INDEX IF EXISTS public."idx-derivation_output-hash-cached";
DROP INDEX IF EXISTS public."idx-derivation_output-cached_path";
ALTER TABLE public.derivation_output DROP CONSTRAINT IF EXISTS "fk-derivation_output-cached_path";
ALTER TABLE public.derivation_output DROP COLUMN is_cached, DROP COLUMN cached_path;
"#;

const DOWN: &str = r#"
ALTER TABLE public.derivation_output
    ADD COLUMN is_cached boolean DEFAULT false NOT NULL,
    ADD COLUMN cached_path uuid;
UPDATE public.derivation_output o
SET is_cached = true, cached_path = cp.id
FROM public.cached_path cp
WHERE cp.hash = o.hash AND cp.file_hash IS NOT NULL;
ALTER TABLE ONLY public.derivation_output
    ADD CONSTRAINT "fk-derivation_output-cached_path" FOREIGN KEY (cached_path) REFERENCES public.cached_path(id) ON DELETE SET NULL;
CREATE INDEX "idx-derivation_output-cached_path" ON public.derivation_output USING btree (cached_path) WHERE (cached_path IS NOT NULL);
CREATE INDEX "idx-derivation_output-hash-cached" ON public.derivation_output USING btree (hash) WHERE (is_cached = true);
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
