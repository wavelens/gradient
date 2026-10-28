/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.exec_job_output (
    exec_job uuid NOT NULL,
    path text NOT NULL,
    size bigint NOT NULL,
    content_type text NOT NULL,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL,
    CONSTRAINT exec_job_output_pkey PRIMARY KEY (exec_job, path),
    CONSTRAINT "fk-exec_job_output-exec_job" FOREIGN KEY (exec_job) REFERENCES public.exec_job(id) ON DELETE CASCADE
);

CREATE TABLE public.exec_job_pull_request (
    id uuid NOT NULL,
    exec_job uuid NOT NULL,
    integration uuid NOT NULL,
    url text NOT NULL,
    branch text NOT NULL,
    number bigint NOT NULL,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT exec_job_pull_request_pkey PRIMARY KEY (id),
    CONSTRAINT "fk-exec_job_pull_request-exec_job" FOREIGN KEY (exec_job) REFERENCES public.exec_job(id) ON DELETE CASCADE,
    CONSTRAINT "fk-exec_job_pull_request-integration" FOREIGN KEY (integration) REFERENCES public.integration(id) ON DELETE CASCADE
);

CREATE TABLE public.exec_job_token (
    id uuid NOT NULL,
    attempt uuid NOT NULL,
    token_hash text NOT NULL,
    project uuid NOT NULL,
    task uuid NOT NULL,
    expires_at timestamp without time zone NOT NULL,
    revoked boolean DEFAULT false NOT NULL,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT exec_job_token_pkey PRIMARY KEY (id),
    CONSTRAINT "uq-exec_job_token-token_hash" UNIQUE (token_hash),
    CONSTRAINT "fk-exec_job_token-attempt" FOREIGN KEY (attempt) REFERENCES public.exec_job_attempt(id) ON DELETE CASCADE
);

CREATE TABLE public.exec_job_continuation (
    exec_job uuid NOT NULL,
    evaluation uuid NOT NULL,
    attributes jsonb NOT NULL,
    status smallint NOT NULL,
    message text,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL,
    CONSTRAINT exec_job_continuation_pkey PRIMARY KEY (exec_job),
    CONSTRAINT "fk-exec_job_continuation-exec_job" FOREIGN KEY (exec_job) REFERENCES public.exec_job(id) ON DELETE CASCADE,
    CONSTRAINT "fk-exec_job_continuation-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE
);

ALTER TABLE public.evaluation ADD COLUMN parent_exec_job uuid;
ALTER TABLE public.evaluation ADD CONSTRAINT "fk-evaluation-parent_exec_job" FOREIGN KEY (parent_exec_job) REFERENCES public.exec_job(id) ON DELETE SET NULL;

CREATE INDEX "idx-exec_job_token-attempt" ON public.exec_job_token USING btree (attempt);
CREATE INDEX "idx-exec_job_continuation-open" ON public.exec_job_continuation USING btree (evaluation) WHERE (status IN (0, 1));
CREATE INDEX "idx-evaluation-parent_exec_job" ON public.evaluation USING btree (parent_exec_job) WHERE (parent_exec_job IS NOT NULL);
CREATE INDEX "idx-exec_job_pull_request-job" ON public.exec_job_pull_request USING btree (exec_job, created_at DESC);
CREATE INDEX "idx-exec_job_pull_request-integration" ON public.exec_job_pull_request USING btree (integration);
"#;

const DOWN: &str = r#"
DROP INDEX public."idx-evaluation-parent_exec_job";
ALTER TABLE public.evaluation DROP COLUMN parent_exec_job;
DROP TABLE public.exec_job_continuation;
DROP TABLE public.exec_job_token;
DROP TABLE public.exec_job_pull_request;
DROP TABLE public.exec_job_output;
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
