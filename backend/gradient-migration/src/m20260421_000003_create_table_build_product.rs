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
                    .table(BuildProduct::Table)
                    .col(
                        ColumnDef::new(BuildProduct::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(BuildProduct::DerivationOutput)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BuildProduct::FileType).string().not_null())
                    .col(ColumnDef::new(BuildProduct::Name).string().not_null())
                    .col(ColumnDef::new(BuildProduct::Path).string().not_null())
                    .col(ColumnDef::new(BuildProduct::Size).big_integer().null())
                    .col(
                        ColumnDef::new(BuildProduct::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_product-derivation_output")
                            .from(BuildProduct::Table, BuildProduct::DerivationOutput)
                            .to(DerivationOutput::Table, DerivationOutput::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-build_product-derivation_output")
                    .table(BuildProduct::Table)
                    .col(BuildProduct::DerivationOutput)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BuildProduct::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum BuildProduct {
    Table,
    Id,
    DerivationOutput,
    FileType,
    Name,
    Path,
    Size,
    CreatedAt,
}

#[derive(DeriveIden)]
enum DerivationOutput {
    Table,
    Id,
}
