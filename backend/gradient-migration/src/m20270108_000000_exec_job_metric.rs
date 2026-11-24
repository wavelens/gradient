/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "CREATE TABLE public.exec_job_metric ( \
    exec_job uuid NOT NULL REFERENCES public.exec_job(id) ON DELETE CASCADE, \
    attempt uuid NOT NULL REFERENCES public.exec_job_attempt(id) ON DELETE CASCADE, \
    sequence integer NOT NULL, \
    received_at timestamp without time zone NOT NULL, \
    window_ms integer NOT NULL, \
    cpu_avg real NOT NULL, \
    cpu_max real NOT NULL, \
    ram_avg_bytes bigint NOT NULL, \
    ram_max_bytes bigint NOT NULL, \
    ram_total_bytes bigint NOT NULL, \
    disk_max_bytes bigint NOT NULL, \
    disk_total_bytes bigint NOT NULL, \
    PRIMARY KEY (attempt, sequence) \
); \
CREATE INDEX \"idx-exec_job_metric-exec_job\" ON public.exec_job_metric USING btree (exec_job);";

const DOWN: &str = "DROP TABLE public.exec_job_metric;";

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
