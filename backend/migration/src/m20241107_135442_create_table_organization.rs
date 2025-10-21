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
                    .table(Organization::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Organization::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Organization::Name)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Organization::DisplayName)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Organization::Description).text().not_null())
                    .col(ColumnDef::new(Organization::PublicKey).string().not_null())
                    .col(
                        ColumnDef::new(Organization::UseNixStore)
                            .boolean()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Organization::PrivateKey).string().not_null())
                    .col(ColumnDef::new(Organization::CreatedBy).uuid().not_null())
                    .col(
                        ColumnDef::new(Organization::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-organization-created_by")
                            .from(Organization::Table, Organization::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Organization::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
    Name,
    DisplayName,
    Description,
    PublicKey,
    PrivateKey,
    UseNixStore,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
