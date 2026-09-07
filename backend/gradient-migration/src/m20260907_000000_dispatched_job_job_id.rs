/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// The scheduler's own job key (`build:<anchor>` / `eval:<evaluation>`), which is
/// unique among in-flight jobs. Closing a dispatch used to guess at
/// (worker, evaluation, newest open), which attached outcomes to the wrong row
/// whenever one worker ran several jobs of one evaluation.
const OPEN_BY_JOB_ID_INDEX: &str = "CREATE INDEX IF NOT EXISTS \"idx-dispatched_job-open-by-job-id\" \
     ON dispatched_job (job_id, dispatched_at DESC) \
     WHERE finished_at IS NULL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DispatchedJob::Table)
                    .add_column_if_not_exists(ColumnDef::new(DispatchedJob::JobId).text().null())
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(OPEN_BY_JOB_ID_INDEX)
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS \"idx-dispatched_job-open-by-job-id\"")
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DispatchedJob::Table)
                    .drop_column(DispatchedJob::JobId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum DispatchedJob {
    Table,
    JobId,
}
