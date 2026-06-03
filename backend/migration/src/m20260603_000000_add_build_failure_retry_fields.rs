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
                    .add_column(
                        ColumnDef::new(Build::Attempt)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .add_column(ColumnDef::new(Build::TimeoutSecs).big_integer().null())
                    .add_column(ColumnDef::new(Build::MaxSilentSecs).big_integer().null())
                    .add_column(
                        ColumnDef::new(Build::PreferLocalBuild)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_column(Build::PreferLocalBuild)
                    .drop_column(Build::MaxSilentSecs)
                    .drop_column(Build::TimeoutSecs)
                    .drop_column(Build::Attempt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Attempt,
    TimeoutSecs,
    MaxSilentSecs,
    PreferLocalBuild,
}
