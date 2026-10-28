/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.project_secret (
    id uuid NOT NULL,
    project uuid NOT NULL,
    name text NOT NULL,
    value text NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    updated_by uuid,
    updated_at timestamp without time zone NOT NULL,
    CONSTRAINT project_secret_pkey PRIMARY KEY (id),
    CONSTRAINT "uq-project_secret-project-name" UNIQUE (project, name),
    CONSTRAINT "fk-project_secret-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE,
    CONSTRAINT "fk-project_secret-updated_by" FOREIGN KEY (updated_by) REFERENCES public."user"(id) ON DELETE SET NULL
);

CREATE INDEX "idx-project_secret-updated_by" ON public.project_secret USING btree (updated_by) WHERE (updated_by IS NOT NULL);
"#;

const DOWN: &str = r#"
DROP TABLE public.project_secret;
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
