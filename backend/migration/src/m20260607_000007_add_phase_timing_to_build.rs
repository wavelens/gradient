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
                    .add_column(ColumnDef::new(Build::ReadyAt).date_time().null())
                    .add_column(ColumnDef::new(Build::DispatchedAt).date_time().null())
                    .add_column(ColumnDef::new(Build::BuildStartedAt).date_time().null())
                    .add_column(ColumnDef::new(Build::BuildFinishedAt).date_time().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_column(Build::ReadyAt)
                    .drop_column(Build::DispatchedAt)
                    .drop_column(Build::BuildStartedAt)
                    .drop_column(Build::BuildFinishedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
#[allow(clippy::enum_variant_names)]
enum Build {
    Table,
    ReadyAt,
    DispatchedAt,
    BuildStartedAt,
    BuildFinishedAt,
}
