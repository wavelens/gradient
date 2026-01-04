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
                    .table(OrganizationUser::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OrganizationUser::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OrganizationUser::Organization)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OrganizationUser::User).uuid().not_null())
                    .col(ColumnDef::new(OrganizationUser::Role).uuid().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-organization_user-organization")
                            .from(OrganizationUser::Table, OrganizationUser::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-organization_user-user")
                            .from(OrganizationUser::Table, OrganizationUser::User)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-organization_user-role")
                            .from(OrganizationUser::Table, OrganizationUser::Role)
                            .to(Role::Table, Role::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OrganizationUser::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum OrganizationUser {
    Table,
    Id,
    Organization,
    User,
    Role,
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

#[derive(DeriveIden)]
enum Role {
    Table,
    Id,
}
