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
                    .table(BuildAttempt::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(BuildAttempt::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(BuildAttempt::Build).uuid().not_null())
                    .col(ColumnDef::new(BuildAttempt::DispatchedJob).uuid().not_null())
                    .col(ColumnDef::new(BuildAttempt::Substitute).boolean().not_null().default(false))
                    .col(ColumnDef::new(BuildAttempt::Outcome).integer().not_null().default(0))
                    .col(ColumnDef::new(BuildAttempt::Reason).integer().null())
                    .col(ColumnDef::new(BuildAttempt::FailureMessage).text().null())
                    .col(ColumnDef::new(BuildAttempt::LogId).uuid().null())
                    .col(ColumnDef::new(BuildAttempt::BuildContext).json_binary().not_null())
                    .col(ColumnDef::new(BuildAttempt::BuildStartedAt).timestamp().null())
                    .col(ColumnDef::new(BuildAttempt::BuildFinishedAt).timestamp().null())
                    .col(ColumnDef::new(BuildAttempt::CreatedAt).timestamp().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_build_attempt_build")
                    .table(BuildAttempt::Table)
                    .col(BuildAttempt::Build)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_build_attempt_dispatched_job")
                    .table(BuildAttempt::Table)
                    .col(BuildAttempt::DispatchedJob)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(BuildAttempt::Table).to_owned()).await
    }
}

#[derive(DeriveIden)]
enum BuildAttempt {
    Table,
    Id,
    Build,
    DispatchedJob,
    Substitute,
    Outcome,
    Reason,
    FailureMessage,
    LogId,
    BuildContext,
    BuildStartedAt,
    BuildFinishedAt,
    CreatedAt,
}
