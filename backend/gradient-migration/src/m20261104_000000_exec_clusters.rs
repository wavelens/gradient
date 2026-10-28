/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.exec_job ADD COLUMN node_index smallint DEFAULT 0 NOT NULL;
DROP INDEX public."uq-exec_job-entry_point";
CREATE UNIQUE INDEX "idx-exec_job-entry_point-node" ON public.exec_job USING btree (entry_point, node_index);
ALTER TABLE public.exec_job_attempt ADD COLUMN master_port integer, ADD COLUMN guest_address text;
ALTER TABLE public.cluster_job ADD COLUMN attempts_outside_budget integer DEFAULT 0 NOT NULL;
ALTER TABLE public.cluster_member
    DROP CONSTRAINT cluster_member_check,
    ADD COLUMN exec_job uuid,
    ADD CONSTRAINT "fk-cluster_member-exec_job" FOREIGN KEY (exec_job) REFERENCES public.exec_job(id) ON DELETE CASCADE,
    ADD CONSTRAINT cluster_member_check CHECK ((num_nonnulls(evaluation, derivation_build, exec_job) = 1));
CREATE UNIQUE INDEX "idx-cluster_member-exec_job" ON public.cluster_member USING btree (exec_job) WHERE (exec_job IS NOT NULL);
"#;

const DOWN: &str = r#"
DELETE FROM public.cluster_job WHERE id IN (SELECT cluster_job FROM public.cluster_member WHERE exec_job IS NOT NULL);
DROP INDEX public."idx-cluster_member-exec_job";
ALTER TABLE public.cluster_member
    DROP CONSTRAINT cluster_member_check,
    DROP COLUMN exec_job,
    ADD CONSTRAINT cluster_member_check CHECK ((num_nonnulls(evaluation, derivation_build) = 1));
ALTER TABLE public.cluster_job DROP COLUMN attempts_outside_budget;
ALTER TABLE public.exec_job_attempt DROP COLUMN guest_address, DROP COLUMN master_port;
DELETE FROM public.exec_job WHERE node_index > 0;
DROP INDEX public."idx-exec_job-entry_point-node";
CREATE UNIQUE INDEX "uq-exec_job-entry_point" ON public.exec_job USING btree (entry_point);
ALTER TABLE public.exec_job DROP COLUMN node_index;
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
