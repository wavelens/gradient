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
                    .table(UserSshKey::Table)
                    .col(
                        ColumnDef::new(UserSshKey::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(UserSshKey::User).uuid().not_null())
                    .col(ColumnDef::new(UserSshKey::Name).text().not_null())
                    .col(ColumnDef::new(UserSshKey::PublicKey).text().not_null())
                    .col(
                        ColumnDef::new(UserSshKey::Fingerprint)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(UserSshKey::CreatedAt).date_time().not_null())
                    .col(ColumnDef::new(UserSshKey::LastUsedAt).date_time())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-user_ssh_key-user")
                            .from(UserSshKey::Table, UserSshKey::User)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-user_ssh_key-user")
                    .table(UserSshKey::Table)
                    .col(UserSshKey::User)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UserSshKey::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum UserSshKey {
    Table,
    Id,
    User,
    Name,
    PublicKey,
    Fingerprint,
    CreatedAt,
    LastUsedAt,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
