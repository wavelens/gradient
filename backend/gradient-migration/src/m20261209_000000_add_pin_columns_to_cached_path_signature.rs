/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "ALTER TABLE public.cached_path_signature \
    ADD COLUMN pinned_at timestamp without time zone, \
    ADD COLUMN pinned_by_user_id uuid, \
    ADD CONSTRAINT \"fk-cached_path_signature-pinned_by_user_id\" FOREIGN KEY (pinned_by_user_id) \
        REFERENCES public.\"user\"(id) ON DELETE SET NULL; \
    CREATE INDEX \"idx-cached_path_signature-pinned_by_user_id\" \
        ON public.cached_path_signature (pinned_by_user_id) WHERE pinned_by_user_id IS NOT NULL;";

const DOWN: &str = "ALTER TABLE public.cached_path_signature \
    DROP COLUMN pinned_by_user_id, \
    DROP COLUMN pinned_at;";

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
