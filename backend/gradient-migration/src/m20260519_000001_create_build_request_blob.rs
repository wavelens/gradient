/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `build_request_blob` is the per-org index of content-addressed source
//! files uploaded by `gradient build`. Payloads live on the configured
//! storage backend (local FS or S3) keyed by BLAKE3 hash; this table is
//! the truth source for "does the org already have this blob?" queries
//! and for TTL-based garbage collection.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(BuildRequestBlob::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BuildRequestBlob::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(BuildRequestBlob::Organization)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BuildRequestBlob::Hash).binary().not_null())
                    .col(
                        ColumnDef::new(BuildRequestBlob::Size)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BuildRequestBlob::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BuildRequestBlob::LastUsedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_request_blob-organization")
                            .from(BuildRequestBlob::Table, BuildRequestBlob::Organization)
                            .to(Alias::new("organization"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ux-build_request_blob-org-hash")
                    .table(BuildRequestBlob::Table)
                    .col(BuildRequestBlob::Organization)
                    .col(BuildRequestBlob::Hash)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BuildRequestBlob::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BuildRequestBlob {
    Table,
    Id,
    Organization,
    Hash,
    Size,
    CreatedAt,
    LastUsedAt,
}
