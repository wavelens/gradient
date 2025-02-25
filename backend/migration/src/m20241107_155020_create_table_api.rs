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
                    .table(Api::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Api::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Api::OwnedBy).uuid().not_null())
                    .col(ColumnDef::new(Api::Name).string().not_null())
                    .col(ColumnDef::new(Api::Key).string().not_null())
                    .col(ColumnDef::new(Api::LastUsedAt).date_time().not_null())
                    .col(ColumnDef::new(Api::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-api-owned_by")
                            .from(Api::Table, Api::OwnedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Api::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Api {
    Table,
    Id,
    OwnedBy,
    Name,
    Key,
    LastUsedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
