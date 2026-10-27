/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.project ADD COLUMN exec_enabled boolean DEFAULT false NOT NULL;
ALTER TABLE public.entry_point ADD COLUMN exec jsonb;

CREATE TABLE public.exec_job (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    entry_point uuid NOT NULL,
    program_derivation uuid NOT NULL,
    image_derivation uuid,
    requirement jsonb NOT NULL,
    spec jsonb NOT NULL,
    retries_left integer NOT NULL,
    status smallint NOT NULL,
    waiting_reason jsonb,
    message text,
    created_at timestamp without time zone NOT NULL,
    started_at timestamp without time zone,
    finished_at timestamp without time zone,
    CONSTRAINT exec_job_pkey PRIMARY KEY (id),
    CONSTRAINT "fk-exec_job-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE,
    CONSTRAINT "fk-exec_job-entry_point" FOREIGN KEY (entry_point) REFERENCES public.entry_point(id) ON DELETE CASCADE,
    CONSTRAINT "fk-exec_job-program_derivation" FOREIGN KEY (program_derivation) REFERENCES public.derivation(id),
    CONSTRAINT "fk-exec_job-image_derivation" FOREIGN KEY (image_derivation) REFERENCES public.derivation(id)
);

CREATE TABLE public.exec_job_attempt (
    id uuid NOT NULL,
    exec_job uuid NOT NULL,
    assignment_id uuid NOT NULL,
    worker text NOT NULL,
    number integer NOT NULL,
    started_at timestamp without time zone NOT NULL,
    ended_at timestamp without time zone,
    end_reason smallint,
    message text,
    CONSTRAINT exec_job_attempt_pkey PRIMARY KEY (id),
    CONSTRAINT "uq-exec_job_attempt-exec_job-number" UNIQUE (exec_job, number),
    CONSTRAINT "fk-exec_job_attempt-exec_job" FOREIGN KEY (exec_job) REFERENCES public.exec_job(id) ON DELETE CASCADE
);

CREATE TABLE public.exec_job_phase (
    id uuid NOT NULL,
    exec_job uuid NOT NULL,
    attempt uuid NOT NULL,
    name text NOT NULL,
    payload bytea NOT NULL,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT exec_job_phase_pkey PRIMARY KEY (id),
    CONSTRAINT "fk-exec_job_phase-exec_job" FOREIGN KEY (exec_job) REFERENCES public.exec_job(id) ON DELETE CASCADE,
    CONSTRAINT "fk-exec_job_phase-attempt" FOREIGN KEY (attempt) REFERENCES public.exec_job_attempt(id) ON DELETE CASCADE
);

CREATE INDEX "idx-exec_job-waiting" ON public.exec_job USING btree (created_at) WHERE (status IN (6, 0));
CREATE INDEX "idx-exec_job-open-by-evaluation" ON public.exec_job USING btree (evaluation) WHERE (status IN (6, 0, 1, 2));
CREATE INDEX "idx-exec_job-evaluation" ON public.exec_job USING btree (evaluation);
CREATE UNIQUE INDEX "uq-exec_job-entry_point" ON public.exec_job USING btree (entry_point);
CREATE INDEX "idx-exec_job-program_derivation" ON public.exec_job USING btree (program_derivation);
CREATE INDEX "idx-exec_job-image_derivation" ON public.exec_job USING btree (image_derivation) WHERE (image_derivation IS NOT NULL);
CREATE INDEX "idx-exec_job_attempt-open" ON public.exec_job_attempt USING btree (exec_job) WHERE (ended_at IS NULL);
CREATE INDEX "idx-exec_job_phase-job" ON public.exec_job_phase USING btree (exec_job, created_at DESC);
CREATE INDEX "idx-exec_job_phase-attempt" ON public.exec_job_phase USING btree (attempt);
CREATE INDEX "idx-entry_point-exec" ON public.entry_point USING btree (evaluation) WHERE (exec IS NOT NULL);
"#;

const DOWN: &str = r#"
DROP INDEX public."idx-entry_point-exec";
DROP TABLE public.exec_job_phase;
DROP TABLE public.exec_job_attempt;
DROP TABLE public.exec_job;
ALTER TABLE public.entry_point DROP COLUMN exec;
ALTER TABLE public.project DROP COLUMN exec_enabled;
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
