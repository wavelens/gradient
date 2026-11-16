/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
CREATE TABLE public.project_git_https_credential (
    id uuid NOT NULL,
    project uuid NOT NULL,
    host text NOT NULL,
    username text NOT NULL,
    password text NOT NULL,
    updated_by uuid,
    updated_at timestamp without time zone NOT NULL,
    CONSTRAINT project_git_https_credential_pkey PRIMARY KEY (id),
    CONSTRAINT "uq-project_git_https_credential-project-host" UNIQUE (project, host),
    CONSTRAINT "fk-project_git_https_credential-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE,
    CONSTRAINT "fk-project_git_https_credential-updated_by" FOREIGN KEY (updated_by) REFERENCES public."user"(id) ON DELETE SET NULL
);

CREATE INDEX "idx-project_git_https_credential-updated_by" ON public.project_git_https_credential USING btree (updated_by) WHERE (updated_by IS NOT NULL);
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
DROP TABLE public.project_git_https_credential;
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
