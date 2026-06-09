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
                    .table(Feature::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Feature::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Feature::Name).string().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Feature::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Feature {
    Table,
    Id,
    Name,
}
