/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Partial index over the rows a job completion has to find: the open dispatch
/// for one worker and evaluation. Written as raw SQL because sea-orm's index
/// builder has no `WHERE` clause.
const OPEN_BY_WORKER_INDEX: &str = "CREATE INDEX IF NOT EXISTS \"idx-dispatched_job-open-by-worker\" \
     ON dispatched_job (worker_id, evaluation_id, dispatched_at DESC) \
     WHERE finished_at IS NULL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DispatchedJobPhase::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DispatchedJobPhase::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DispatchedJobPhase::DispatchedJob)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DispatchedJobPhase::Seq).integer().not_null())
                    .col(
                        ColumnDef::new(DispatchedJobPhase::ParentSeq)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DispatchedJobPhase::Phase)
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DispatchedJobPhase::StartMs)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DispatchedJobPhase::EndMs)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DispatchedJobPhase::Paths)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DispatchedJobPhase::Bytes)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DispatchedJobPhase::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-dispatched_job_phase-dispatched_job")
                            .from(DispatchedJobPhase::Table, DispatchedJobPhase::DispatchedJob)
                            .to(DispatchedJob::Table, DispatchedJob::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-dispatched_job_phase-job-seq")
                    .table(DispatchedJobPhase::Table)
                    .col(DispatchedJobPhase::DispatchedJob)
                    .col(DispatchedJobPhase::Seq)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-dispatched_job_phase-phase-created_at")
                    .table(DispatchedJobPhase::Table)
                    .col(DispatchedJobPhase::Phase)
                    .col(DispatchedJobPhase::CreatedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DispatchedJob::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(DispatchedJob::Outcome)
                            .small_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(OPEN_BY_WORKER_INDEX)
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS \"idx-dispatched_job-open-by-worker\"")
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DispatchedJob::Table)
                    .drop_column(DispatchedJob::Outcome)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(DispatchedJobPhase::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DispatchedJobPhase {
    Table,
    Id,
    DispatchedJob,
    Seq,
    ParentSeq,
    Phase,
    StartMs,
    EndMs,
    Paths,
    Bytes,
    CreatedAt,
}

#[derive(DeriveIden)]
enum DispatchedJob {
    Table,
    Id,
    Outcome,
}
