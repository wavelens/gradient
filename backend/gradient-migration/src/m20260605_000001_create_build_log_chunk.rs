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
                    .table(BuildLogChunk::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BuildLogChunk::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(BuildLogChunk::Build).uuid().not_null())
                    .col(
                        ColumnDef::new(BuildLogChunk::ChunkIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BuildLogChunk::ByteStart)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BuildLogChunk::ByteLen).integer().not_null())
                    .col(
                        ColumnDef::new(BuildLogChunk::LineStart)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BuildLogChunk::LineCount).integer().not_null())
                    .col(
                        ColumnDef::new(BuildLogChunk::CompressedSize)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BuildLogChunk::ColorPrefix).text().not_null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_build_log_chunk_build_index")
                    .table(BuildLogChunk::Table)
                    .col(BuildLogChunk::Build)
                    .col(BuildLogChunk::ChunkIndex)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BuildLogChunk::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BuildLogChunk {
    Table,
    Id,
    Build,
    ChunkIndex,
    ByteStart,
    ByteLen,
    LineStart,
    LineCount,
    CompressedSize,
    ColorPrefix,
}
