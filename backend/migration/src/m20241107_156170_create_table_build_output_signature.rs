/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
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
                    .table(BuildOutputSignature::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BuildOutputSignature::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(BuildOutputSignature::BuildOutput)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BuildOutputSignature::Cache)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BuildOutputSignature::Signature).string())
                    .col(
                        ColumnDef::new(BuildOutputSignature::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_output_signature-build_output")
                            .from(
                                BuildOutputSignature::Table,
                                BuildOutputSignature::BuildOutput,
                            )
                            .to(BuildOutput::Table, BuildOutput::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_output_signature-cache")
                            .from(BuildOutputSignature::Table, BuildOutputSignature::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BuildOutputSignature::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BuildOutputSignature {
    Table,
    Id,
    BuildOutput,
    Cache,
    Signature,
    CreatedAt,
}

#[derive(DeriveIden)]
enum BuildOutput {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}
