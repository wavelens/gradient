/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DispatchedJob::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(DispatchedJob::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(DispatchedJob::Kind).small_integer().not_null())
                    .col(ColumnDef::new(DispatchedJob::BuildId).uuid().null())
                    .col(ColumnDef::new(DispatchedJob::EvaluationId).uuid().not_null())
                    .col(ColumnDef::new(DispatchedJob::Organization).uuid().not_null())
                    .col(ColumnDef::new(DispatchedJob::Project).uuid().null())
                    .col(ColumnDef::new(DispatchedJob::Derivation).uuid().null())
                    .col(ColumnDef::new(DispatchedJob::WorkerId).string().not_null())
                    .col(ColumnDef::new(DispatchedJob::Score).double().not_null().default(0.0))
                    .col(ColumnDef::new(DispatchedJob::QueuedAt).date_time().not_null())
                    .col(ColumnDef::new(DispatchedJob::ReadyAt).date_time().null())
                    .col(ColumnDef::new(DispatchedJob::DispatchedAt).date_time().not_null())
                    .col(ColumnDef::new(DispatchedJob::FinishedAt).date_time().null())
                    .col(ColumnDef::new(DispatchedJob::Outcome).small_integer().null())
                    .col(ColumnDef::new(DispatchedJob::ScoreBreakdown).json_binary().not_null())
                    .col(ColumnDef::new(DispatchedJob::WorkerContext).json_binary().not_null())
                    .col(ColumnDef::new(DispatchedJob::JobContext).json_binary().not_null())
                    .col(ColumnDef::new(DispatchedJob::Candidates).json_binary().null())
                    .col(ColumnDef::new(DispatchedJob::CreatedAt).date_time().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-dispatched_job-org-dispatched_at")
                    .table(DispatchedJob::Table)
                    .col(DispatchedJob::Organization)
                    .col((DispatchedJob::DispatchedAt, IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-dispatched_job-worker-dispatched_at")
                    .table(DispatchedJob::Table)
                    .col(DispatchedJob::WorkerId)
                    .col((DispatchedJob::DispatchedAt, IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;

        // Partial index for the live (open) jobs view; sea-query has no WHERE
        // clause builder for indexes, so issue raw SQL (Postgres).
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS \"idx-dispatched_job-open\" \
                 ON \"dispatched_job\" (\"dispatched_at\" DESC) \
                 WHERE \"finished_at\" IS NULL",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DispatchedJob::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DispatchedJob {
    Table,
    Id,
    Kind,
    BuildId,
    EvaluationId,
    Organization,
    Project,
    Derivation,
    WorkerId,
    Score,
    QueuedAt,
    ReadyAt,
    DispatchedAt,
    FinishedAt,
    Outcome,
    ScoreBreakdown,
    WorkerContext,
    JobContext,
    Candidates,
    CreatedAt,
}
