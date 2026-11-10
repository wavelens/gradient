/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const DROP: &str =
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-dispatched_job-job_id-dispatched_at\";";

const CREATE: &str = "CREATE INDEX CONCURRENTLY \"idx-dispatched_job-job_id-dispatched_at\" \
    ON public.dispatched_job USING btree (job_id, dispatched_at DESC);";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(DROP).await?;
        db.execute_unprepared(CREATE).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DROP).await?;
        Ok(())
    }
}
