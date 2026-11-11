/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.evaluation
    ADD COLUMN abort_reason integer,
    ADD COLUMN aborted_by uuid,
    ADD COLUMN superseded_by uuid,
    ADD CONSTRAINT "fk-evaluation-aborted_by" FOREIGN KEY (aborted_by) REFERENCES public."user"(id) ON DELETE SET NULL,
    ADD CONSTRAINT "fk-evaluation-superseded_by" FOREIGN KEY (superseded_by) REFERENCES public.evaluation(id) ON DELETE SET NULL;

ALTER TABLE public.commit
    ADD COLUMN project uuid,
    ADD COLUMN fetched_at timestamp without time zone,
    ADD CONSTRAINT "fk-commit-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE SET NULL;

UPDATE public.commit c
SET project = t.project
FROM public.evaluation e
JOIN public.task t ON t.id = e.task
WHERE e.commit = c.id
  AND e.kind <> 3
  AND NOT (t.managed AND t.name = 'build-request')
  AND btrim(c.hash, '\x00'::bytea) <> '\x'::bytea;

CREATE TEMP TABLE commit_merge AS
SELECT id, first_value(id) OVER (
    PARTITION BY project, hash
    ORDER BY (message <> '') DESC, id DESC
) AS keep
FROM public.commit
WHERE project IS NOT NULL;

UPDATE public.evaluation e
SET commit = m.keep
FROM commit_merge m
WHERE e.commit = m.id AND m.id <> m.keep;

DELETE FROM public.commit c
USING commit_merge m
WHERE c.id = m.id AND m.id <> m.keep;

DROP TABLE commit_merge;

CREATE UNIQUE INDEX "uq-commit-project-hash" ON public.commit USING btree (project, hash);
"#;

const DOWN: &str = r#"
DROP INDEX public."uq-commit-project-hash";

ALTER TABLE public.commit
    DROP CONSTRAINT "fk-commit-project",
    DROP COLUMN fetched_at,
    DROP COLUMN project;

ALTER TABLE public.evaluation
    DROP CONSTRAINT "fk-evaluation-superseded_by",
    DROP CONSTRAINT "fk-evaluation-aborted_by",
    DROP COLUMN superseded_by,
    DROP COLUMN aborted_by,
    DROP COLUMN abort_reason;
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
