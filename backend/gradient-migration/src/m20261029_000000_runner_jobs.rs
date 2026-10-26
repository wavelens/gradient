/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.project ADD COLUMN runner_enabled boolean DEFAULT false NOT NULL;
ALTER TABLE public.entry_point ADD COLUMN runner jsonb;

CREATE TABLE public.runner_job (
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
    CONSTRAINT runner_job_pkey PRIMARY KEY (id),
    CONSTRAINT "fk-runner_job-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE,
    CONSTRAINT "fk-runner_job-entry_point" FOREIGN KEY (entry_point) REFERENCES public.entry_point(id) ON DELETE CASCADE,
    CONSTRAINT "fk-runner_job-program_derivation" FOREIGN KEY (program_derivation) REFERENCES public.derivation(id),
    CONSTRAINT "fk-runner_job-image_derivation" FOREIGN KEY (image_derivation) REFERENCES public.derivation(id)
);

CREATE TABLE public.runner_job_attempt (
    id uuid NOT NULL,
    runner_job uuid NOT NULL,
    assignment_id uuid NOT NULL,
    worker text NOT NULL,
    number integer NOT NULL,
    started_at timestamp without time zone NOT NULL,
    ended_at timestamp without time zone,
    end_reason smallint,
    message text,
    CONSTRAINT runner_job_attempt_pkey PRIMARY KEY (id),
    CONSTRAINT "uq-runner_job_attempt-runner_job-number" UNIQUE (runner_job, number),
    CONSTRAINT "fk-runner_job_attempt-runner_job" FOREIGN KEY (runner_job) REFERENCES public.runner_job(id) ON DELETE CASCADE
);

CREATE TABLE public.runner_job_phase (
    id uuid NOT NULL,
    runner_job uuid NOT NULL,
    attempt uuid NOT NULL,
    name text NOT NULL,
    payload bytea NOT NULL,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT runner_job_phase_pkey PRIMARY KEY (id),
    CONSTRAINT "fk-runner_job_phase-runner_job" FOREIGN KEY (runner_job) REFERENCES public.runner_job(id) ON DELETE CASCADE,
    CONSTRAINT "fk-runner_job_phase-attempt" FOREIGN KEY (attempt) REFERENCES public.runner_job_attempt(id) ON DELETE CASCADE
);

CREATE INDEX "idx-runner_job-waiting" ON public.runner_job USING btree (created_at) WHERE (status IN (6, 0));
CREATE INDEX "idx-runner_job-open-by-evaluation" ON public.runner_job USING btree (evaluation) WHERE (status IN (6, 0, 1, 2));
CREATE INDEX "idx-runner_job-evaluation" ON public.runner_job USING btree (evaluation);
CREATE UNIQUE INDEX "uq-runner_job-entry_point" ON public.runner_job USING btree (entry_point);
CREATE INDEX "idx-runner_job-program_derivation" ON public.runner_job USING btree (program_derivation);
CREATE INDEX "idx-runner_job-image_derivation" ON public.runner_job USING btree (image_derivation) WHERE (image_derivation IS NOT NULL);
CREATE INDEX "idx-runner_job_attempt-open" ON public.runner_job_attempt USING btree (runner_job) WHERE (ended_at IS NULL);
CREATE INDEX "idx-runner_job_phase-job" ON public.runner_job_phase USING btree (runner_job, created_at DESC);
CREATE INDEX "idx-runner_job_phase-attempt" ON public.runner_job_phase USING btree (attempt);
CREATE INDEX "idx-entry_point-runner" ON public.entry_point USING btree (evaluation) WHERE (runner IS NOT NULL);
"#;

const DOWN: &str = r#"
DROP INDEX public."idx-entry_point-runner";
DROP TABLE public.runner_job_phase;
DROP TABLE public.runner_job_attempt;
DROP TABLE public.runner_job;
ALTER TABLE public.entry_point DROP COLUMN runner;
ALTER TABLE public.project DROP COLUMN runner_enabled;
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
