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
                    .table(Webhook::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Webhook::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Webhook::Organization).uuid().not_null())
                    .col(ColumnDef::new(Webhook::Name).string().not_null())
                    .col(ColumnDef::new(Webhook::Url).text().not_null())
                    .col(ColumnDef::new(Webhook::Secret).text().not_null())
                    .col(ColumnDef::new(Webhook::Events).json().not_null())
                    .col(
                        ColumnDef::new(Webhook::Active)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(Webhook::CreatedBy).uuid().not_null())
                    .col(ColumnDef::new(Webhook::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-webhook-organization")
                            .from(Webhook::Table, Webhook::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-webhook-created_by")
                            .from(Webhook::Table, Webhook::CreatedBy)
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
            .drop_table(Table::drop().table(Webhook::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Webhook {
    Table,
    Id,
    Organization,
    Name,
    Url,
    Secret,
    Events,
    Active,
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
