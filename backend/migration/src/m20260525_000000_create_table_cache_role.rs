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
                    .table(CacheRole::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(CacheRole::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(CacheRole::Name).string().not_null())
                    .col(ColumnDef::new(CacheRole::Cache).uuid())
                    .col(ColumnDef::new(CacheRole::Permission).big_integer().not_null())
                    .col(
                        ColumnDef::new(CacheRole::Managed)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_role-cache")
                            .from(CacheRole::Table, CacheRole::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CacheRole::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CacheRole {
    Table,
    Id,
    Name,
    Cache,
    Permission,
    Managed,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}
