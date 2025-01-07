/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
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
                    .table(Commit::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Commit::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Commit::Message).string().not_null())
                    .col(ColumnDef::new(Commit::Hash).blob().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Commit::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Commit {
    Table,
    Id,
    Message,
    Hash,
}
