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
                    .table(BuildDependency::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BuildDependency::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(BuildDependency::Build).uuid().not_null())
                    .col(
                        ColumnDef::new(BuildDependency::Dependency)
                            .uuid()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_dependency-build")
                            .from(BuildDependency::Table, BuildDependency::Build)
                            .to(Build::Table, Build::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_dependency-dependency")
                            .from(BuildDependency::Table, BuildDependency::Dependency)
                            .to(Build::Table, Build::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BuildDependency::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BuildDependency {
    Table,
    Id,
    Build,
    Dependency,
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Id,
}
