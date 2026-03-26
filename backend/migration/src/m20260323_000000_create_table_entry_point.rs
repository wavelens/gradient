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
                    .table(EntryPoint::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EntryPoint::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(EntryPoint::Project).uuid().not_null())
                    .col(ColumnDef::new(EntryPoint::Evaluation).uuid().not_null())
                    .col(ColumnDef::new(EntryPoint::Build).uuid().not_null())
                    .col(ColumnDef::new(EntryPoint::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-entry_point-project")
                            .from(EntryPoint::Table, EntryPoint::Project)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-entry_point-evaluation")
                            .from(EntryPoint::Table, EntryPoint::Evaluation)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-entry_point-build")
                            .from(EntryPoint::Table, EntryPoint::Build)
                            .to(Build::Table, Build::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EntryPoint::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum EntryPoint {
    Table,
    Id,
    Project,
    Evaluation,
    Build,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Id,
}
