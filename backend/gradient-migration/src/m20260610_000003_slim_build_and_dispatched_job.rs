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
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .add_column(ColumnDef::new(Build::Substitutable).boolean().not_null().default(false))
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared("UPDATE build SET substitutable = external_cached;")
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_column(Build::ExternalCached)
                    .drop_column(Build::LogId)
                    .drop_column(Build::BuildTimeMs)
                    .drop_column(Build::Worker)
                    .drop_column(Build::BuildStartedAt)
                    .drop_column(Build::BuildFinishedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DispatchedJob::Table)
                    .drop_column(DispatchedJob::BuildId)
                    .drop_column(DispatchedJob::Derivation)
                    .drop_column(DispatchedJob::Outcome)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DispatchedJob::Table)
                    .add_column(ColumnDef::new(DispatchedJob::BuildId).uuid().null())
                    .add_column(ColumnDef::new(DispatchedJob::Derivation).uuid().null())
                    .add_column(ColumnDef::new(DispatchedJob::Outcome).small_integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .add_column(ColumnDef::new(Build::ExternalCached).boolean().not_null().default(false))
                    .add_column(ColumnDef::new(Build::LogId).uuid().null())
                    .add_column(ColumnDef::new(Build::BuildTimeMs).big_integer().null())
                    .add_column(ColumnDef::new(Build::Worker).text().null())
                    .add_column(ColumnDef::new(Build::BuildStartedAt).timestamp().null())
                    .add_column(ColumnDef::new(Build::BuildFinishedAt).timestamp().null())
                    .drop_column(Build::Substitutable)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
#[allow(clippy::enum_variant_names)]
enum Build {
    Table,
    Substitutable,
    ExternalCached,
    LogId,
    BuildTimeMs,
    Worker,
    BuildStartedAt,
    BuildFinishedAt,
}

#[derive(DeriveIden)]
enum DispatchedJob {
    Table,
    BuildId,
    Derivation,
    Outcome,
}
