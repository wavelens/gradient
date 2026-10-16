/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.webhook
    ADD COLUMN team uuid NULL REFERENCES public.team(id) ON DELETE CASCADE;

ALTER TABLE public.webhook DROP CONSTRAINT "chk-webhook-scope";
ALTER TABLE public.webhook ADD CONSTRAINT "chk-webhook-scope" CHECK (
    ((scope = 0) AND (project IS NOT NULL) AND (cache IS NULL) AND (team IS NULL))
    OR ((scope = 1) AND (cache IS NOT NULL) AND (project IS NULL) AND (team IS NULL))
    OR ((scope = 2) AND (project IS NULL) AND (cache IS NULL) AND (team IS NULL))
    OR ((scope = 3) AND (team IS NOT NULL) AND (project IS NULL) AND (cache IS NULL))
);

DROP INDEX public."idx-webhook-name";
CREATE UNIQUE INDEX "idx-webhook-name" ON public.webhook USING btree (scope, COALESCE(project, cache, team), name);

CREATE INDEX "idx-webhook-team" ON public.webhook USING btree (team) WHERE (team IS NOT NULL);
"#;

const DOWN: &str = r#"
DELETE FROM public.webhook WHERE scope = 3;

DROP INDEX public."idx-webhook-team";

DROP INDEX public."idx-webhook-name";
CREATE UNIQUE INDEX "idx-webhook-name" ON public.webhook USING btree (scope, COALESCE(project, cache), name);

ALTER TABLE public.webhook DROP CONSTRAINT "chk-webhook-scope";
ALTER TABLE public.webhook ADD CONSTRAINT "chk-webhook-scope" CHECK ((((scope = 0) AND (project IS NOT NULL) AND (cache IS NULL)) OR ((scope = 1) AND (cache IS NOT NULL) AND (project IS NULL)) OR ((scope = 2) AND (project IS NULL) AND (cache IS NULL))));

ALTER TABLE public.webhook DROP COLUMN team;
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
