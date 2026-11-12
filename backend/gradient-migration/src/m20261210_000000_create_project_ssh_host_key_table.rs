/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.project_ssh_host_key (
    id uuid NOT NULL,
    project uuid NOT NULL,
    host text NOT NULL,
    port integer NOT NULL,
    public_key text NOT NULL,
    learned_at timestamp without time zone NOT NULL,
    changed_at timestamp without time zone,
    CONSTRAINT project_ssh_host_key_pkey PRIMARY KEY (id),
    CONSTRAINT "uq-project_ssh_host_key-project-host-port" UNIQUE (project, host, port),
    CONSTRAINT "fk-project_ssh_host_key-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE
);
"#;

const DOWN: &str = r#"
DROP TABLE public.project_ssh_host_key;
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
