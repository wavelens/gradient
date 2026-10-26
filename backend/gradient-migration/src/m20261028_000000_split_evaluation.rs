/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.eval_split (
    evaluation uuid NOT NULL,
    cluster_job uuid NOT NULL,
    source_store_path text,
    fingerprint text,
    planned_at timestamp without time zone,
    CONSTRAINT eval_split_pkey PRIMARY KEY (evaluation),
    CONSTRAINT eval_split_evaluation_fkey FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE,
    CONSTRAINT eval_split_cluster_job_fkey FOREIGN KEY (cluster_job) REFERENCES public.cluster_job(id) ON DELETE CASCADE
);
CREATE INDEX "idx-eval_split-cluster_job" ON public.eval_split USING btree (cluster_job);
CREATE TABLE public.eval_shard (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    seq integer NOT NULL,
    pattern text NOT NULL,
    children text[],
    predicted_ms double precision NOT NULL,
    state smallint NOT NULL,
    attempts smallint DEFAULT 0 NOT NULL,
    reported_by_job text,
    taken_job text,
    created_at timestamp without time zone NOT NULL,
    taken_at timestamp without time zone,
    finished_at timestamp without time zone,
    CONSTRAINT eval_shard_pkey PRIMARY KEY (id),
    CONSTRAINT eval_shard_evaluation_fkey FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX "uq-eval_shard-evaluation-seq" ON public.eval_shard USING btree (evaluation, seq);
CREATE INDEX "idx-eval_shard-open" ON public.eval_shard USING btree (evaluation, seq) WHERE (state = ANY (ARRAY[0, 1]));
CREATE INDEX "idx-eval_shard-running-taken_job" ON public.eval_shard USING btree (taken_job) WHERE (state = 1);
ALTER TABLE public.cluster_member ADD COLUMN eval_index smallint, ADD COLUMN min_free_ram_mb integer;
DROP INDEX public."idx-cluster_member-evaluation";
CREATE UNIQUE INDEX "idx-cluster_member-evaluation" ON public.cluster_member USING btree (evaluation, (COALESCE(eval_index, (0)::smallint))) WHERE (evaluation IS NOT NULL);
ALTER TABLE public.cluster_job ADD COLUMN dissolve_after timestamp without time zone;
"#;

const DOWN: &str = r#"
ALTER TABLE public.cluster_job DROP COLUMN dissolve_after;
DELETE FROM public.cluster_member WHERE eval_index > 0;
DROP INDEX public."idx-cluster_member-evaluation";
CREATE UNIQUE INDEX "idx-cluster_member-evaluation" ON public.cluster_member USING btree (evaluation) WHERE (evaluation IS NOT NULL);
ALTER TABLE public.cluster_member DROP COLUMN min_free_ram_mb, DROP COLUMN eval_index;
DROP TABLE public.eval_shard;
DROP TABLE public.eval_split;
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
