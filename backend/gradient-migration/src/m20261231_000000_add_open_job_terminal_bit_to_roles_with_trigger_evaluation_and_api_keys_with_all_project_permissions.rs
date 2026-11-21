/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "SET LOCAL lock_timeout = '5s'; \
    UPDATE public.role SET permission = permission | 262144 \
    WHERE (permission & 4096) <> 0; \
    UPDATE public.api SET permission = permission | 262144 \
    WHERE (permission & 32767) = 32767;";

const DOWN: &str = "SET LOCAL lock_timeout = '5s'; \
    UPDATE public.role SET permission = permission & ~262144; \
    UPDATE public.api SET permission = permission & ~262144;";

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
