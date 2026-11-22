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
    DROP CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url;
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.git_host_connection
    ADD CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url
    CHECK (git_host_type NOT IN (0, 1, 2) OR base_url IS NOT NULL) NOT VALID;
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
