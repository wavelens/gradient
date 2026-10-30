/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "UPDATE public.task t SET exec_enabled = false FROM public.project p \
    WHERE p.id = t.project AND t.exec_enabled AND NOT p.exec_enabled; \
    ALTER TABLE public.project DROP COLUMN exec_enabled;";

const DOWN: &str = "ALTER TABLE public.project ADD COLUMN exec_enabled boolean DEFAULT false NOT NULL; \
    UPDATE public.project p SET exec_enabled = true \
    WHERE EXISTS (SELECT 1 FROM public.task t WHERE t.project = p.id AND t.exec_enabled);";

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
