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
                    .table(DirectBuild::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DirectBuild::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DirectBuild::Organization).uuid().not_null())
                    .col(ColumnDef::new(DirectBuild::Evaluation).uuid().not_null())
                    .col(ColumnDef::new(DirectBuild::Derivation).string().not_null())
                    .col(
                        ColumnDef::new(DirectBuild::RepositoryPath)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DirectBuild::CreatedBy).uuid().not_null())
                    .col(
                        ColumnDef::new(DirectBuild::CreatedAt)
                            .timestamp()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-direct_build-organization")
                            .from(DirectBuild::Table, DirectBuild::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-direct_build-evaluation")
                            .from(DirectBuild::Table, DirectBuild::Evaluation)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-direct_build-created_by")
                            .from(DirectBuild::Table, DirectBuild::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DirectBuild::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DirectBuild {
    Table,
    Id,
    Organization,
    Evaluation,
    Derivation,
    RepositoryPath,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
