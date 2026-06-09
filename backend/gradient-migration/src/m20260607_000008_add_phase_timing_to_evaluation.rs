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
                    .table(Evaluation::Table)
                    .add_column(ColumnDef::new(Evaluation::FetchStartedAt).date_time().null())
                    .add_column(ColumnDef::new(Evaluation::EvalFlakeStartedAt).date_time().null())
                    .add_column(ColumnDef::new(Evaluation::EvalDrvStartedAt).date_time().null())
                    .add_column(ColumnDef::new(Evaluation::BuildingStartedAt).date_time().null())
                    .add_column(ColumnDef::new(Evaluation::FinishedAt).date_time().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .drop_column(Evaluation::FetchStartedAt)
                    .drop_column(Evaluation::EvalFlakeStartedAt)
                    .drop_column(Evaluation::EvalDrvStartedAt)
                    .drop_column(Evaluation::BuildingStartedAt)
                    .drop_column(Evaluation::FinishedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    FetchStartedAt,
    EvalFlakeStartedAt,
    EvalDrvStartedAt,
    BuildingStartedAt,
    FinishedAt,
}
