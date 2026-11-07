/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "ALTER TABLE public.task \
    ADD COLUMN last_fetch_at timestamp without time zone, \
    ADD COLUMN last_fetch_ok_at timestamp without time zone, \
    ADD COLUMN last_fetch_error text;";

const DOWN: &str = "ALTER TABLE public.task \
    DROP COLUMN last_fetch_error, \
    DROP COLUMN last_fetch_ok_at, \
    DROP COLUMN last_fetch_at;";

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
