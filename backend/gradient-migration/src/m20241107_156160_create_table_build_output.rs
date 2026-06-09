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
                    .table(BuildOutput::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BuildOutput::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(BuildOutput::Build).uuid().not_null())
                    .col(ColumnDef::new(BuildOutput::Name).string().not_null())
                    .col(ColumnDef::new(BuildOutput::Output).string().not_null())
                    .col(ColumnDef::new(BuildOutput::Hash).string().not_null())
                    .col(ColumnDef::new(BuildOutput::Package).string().not_null())
                    .col(ColumnDef::new(BuildOutput::FileHash).string())
                    .col(ColumnDef::new(BuildOutput::FileSize).big_integer())
                    .col(ColumnDef::new(BuildOutput::IsCached).boolean().not_null())
                    .col(ColumnDef::new(BuildOutput::Ca).string())
                    .col(
                        ColumnDef::new(BuildOutput::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_output-build")
                            .from(BuildOutput::Table, BuildOutput::Build)
                            .to(Build::Table, Build::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BuildOutput::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BuildOutput {
    Table,
    Id,
    Build,
    Name,
    Output,
    Hash,
    Package,
    FileHash,
    FileSize,
    IsCached,
    Ca,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Id,
}
