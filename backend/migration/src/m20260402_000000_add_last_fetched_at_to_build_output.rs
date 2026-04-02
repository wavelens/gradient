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
                    .table(BuildOutput::Table)
                    .add_column(ColumnDef::new(BuildOutput::LastFetchedAt).date_time().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(BuildOutput::Table)
                    .drop_column(BuildOutput::LastFetchedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum BuildOutput {
    Table,
    LastFetchedAt,
}
