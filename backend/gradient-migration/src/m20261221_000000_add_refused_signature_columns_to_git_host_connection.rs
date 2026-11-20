/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.git_host_connection
    ADD COLUMN refused_signature_count bigint NOT NULL DEFAULT 0,
    ADD COLUMN last_refused_signature_at timestamp without time zone NULL;
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.git_host_connection
    DROP COLUMN refused_signature_count,
    DROP COLUMN last_refused_signature_at;
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
