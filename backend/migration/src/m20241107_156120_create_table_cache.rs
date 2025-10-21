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
                    .table(Cache::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Cache::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Cache::Name).string().not_null().unique_key())
                    .col(ColumnDef::new(Cache::DisplayName).string().not_null())
                    .col(ColumnDef::new(Cache::Description).text().not_null())
                    .col(ColumnDef::new(Cache::Active).boolean().not_null())
                    .col(ColumnDef::new(Cache::Priority).integer().not_null())
                    .col(ColumnDef::new(Cache::SigningKey).string().not_null())
                    .col(ColumnDef::new(Cache::CreatedBy).uuid().not_null())
                    .col(ColumnDef::new(Cache::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache-created_by")
                            .from(Cache::Table, Cache::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Cache::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
    Name,
    DisplayName,
    Description,
    Active,
    Priority,
    SigningKey,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
