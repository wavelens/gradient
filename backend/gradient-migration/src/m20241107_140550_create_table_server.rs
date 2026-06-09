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
                    .table(Server::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Server::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Server::Name).string().not_null())
                    .col(ColumnDef::new(Server::DisplayName).string().not_null())
                    .col(ColumnDef::new(Server::Organization).uuid().not_null())
                    .col(ColumnDef::new(Server::Active).boolean().not_null())
                    .col(ColumnDef::new(Server::Host).string().not_null())
                    .col(ColumnDef::new(Server::Port).integer().not_null())
                    .col(ColumnDef::new(Server::Username).string().not_null())
                    .col(
                        ColumnDef::new(Server::LastConnectionAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Server::CreatedBy).uuid().not_null())
                    .col(ColumnDef::new(Server::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-server-organization")
                            .from(Server::Table, Server::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-server-created_by")
                            .from(Server::Table, Server::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Server::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Server {
    Table,
    Id,
    Name,
    DisplayName,
    Organization,
    Active,
    Host,
    Port,
    Username,
    LastConnectionAt,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
