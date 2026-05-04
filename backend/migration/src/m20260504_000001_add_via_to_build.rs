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
                    .add_column(ColumnDef::new(Build::Via).uuid().null())
                    .add_foreign_key(
                        &TableForeignKey::new()
                            .name("fk-build-via")
                            .from_tbl(Build::Table)
                            .from_col(Build::Via)
                            .to_tbl(Build::Table)
                            .to_col(Build::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-build-via")
                    .table(Build::Table)
                    .col(Build::Via)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(Index::drop().name("idx-build-via").table(Build::Table).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_foreign_key(Alias::new("fk-build-via"))
                    .drop_column(Build::Via)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Id,
    Via,
}
