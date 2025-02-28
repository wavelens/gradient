/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
                    .col(ColumnDef::new(Cache::Priority).integer().not_null())
                    .col(ColumnDef::new(Cache::SigningKey).string().not_null())
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
    Priority,
    SigningKey,
}
