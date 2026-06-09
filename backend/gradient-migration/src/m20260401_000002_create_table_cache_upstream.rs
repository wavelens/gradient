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
                    .table(CacheUpstream::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CacheUpstream::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CacheUpstream::Cache).uuid().not_null())
                    .col(
                        ColumnDef::new(CacheUpstream::DisplayName)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CacheUpstream::Mode)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(CacheUpstream::UpstreamCache).uuid().null())
                    .col(ColumnDef::new(CacheUpstream::Url).string().null())
                    .col(ColumnDef::new(CacheUpstream::PublicKey).string().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_upstream-cache")
                            .from(CacheUpstream::Table, CacheUpstream::Cache)
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
            .drop_table(Table::drop().table(CacheUpstream::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CacheUpstream {
    Table,
    Id,
    Cache,
    DisplayName,
    Mode,
    UpstreamCache,
    Url,
    PublicKey,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}
