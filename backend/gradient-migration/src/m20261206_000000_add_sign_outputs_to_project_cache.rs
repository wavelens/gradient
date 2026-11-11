/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.project_cache ADD COLUMN managed boolean DEFAULT false NOT NULL;
ALTER TABLE public.project_cache ADD COLUMN sign_outputs boolean DEFAULT true NOT NULL;
UPDATE public.project_cache pc
SET sign_outputs = COALESCE(
    (SELECT bool_or(t.sign_cache) FROM public.task t WHERE t.project = pc.project), true);
ALTER TABLE public.task DROP COLUMN sign_cache;
"#;

const DOWN: &str = r#"
ALTER TABLE public.task ADD COLUMN sign_cache boolean DEFAULT true NOT NULL;
UPDATE public.task t
SET sign_cache = COALESCE(
    (SELECT bool_or(pc.sign_outputs) FROM public.project_cache pc WHERE pc.project = t.project),
    true);
ALTER TABLE public.project_cache DROP COLUMN sign_outputs;
ALTER TABLE public.project_cache DROP COLUMN managed;
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
