/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.task_flake_input_source (
    task uuid NOT NULL,
    input_name text NOT NULL,
    repository_url text NOT NULL,
    branch text,
    updated_at timestamp without time zone NOT NULL,
    CONSTRAINT task_flake_input_source_pkey PRIMARY KEY (task, input_name),
    CONSTRAINT "fk-task_flake_input_source-task" FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE
);
"#;

const DOWN: &str = r#"
DROP TABLE public.task_flake_input_source;
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
