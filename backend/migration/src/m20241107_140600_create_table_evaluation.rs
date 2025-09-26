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
                    .table(Evaluation::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Evaluation::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Evaluation::Project).uuid().not_null())
                    .col(ColumnDef::new(Evaluation::Repository).string().not_null())
                    .col(ColumnDef::new(Evaluation::Commit).uuid().not_null())
                    .col(ColumnDef::new(Evaluation::Wildcard).string().not_null())
                    .col(ColumnDef::new(Evaluation::Status).integer().not_null())
                    .col(ColumnDef::new(Evaluation::Previous).uuid())
                    .col(ColumnDef::new(Evaluation::Next).uuid())
                    .col(ColumnDef::new(Evaluation::CreatedAt).date_time().not_null())
                    .col(ColumnDef::new(Evaluation::Error).text())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-evaluation-project")
                            .from(Evaluation::Table, Evaluation::Project)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-evaluation-previous")
                            .from(Evaluation::Table, Evaluation::Previous)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-evaluation-next")
                            .from(Evaluation::Table, Evaluation::Next)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Evaluation::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
    Project,
    Repository,
    Commit,
    Wildcard,
    Status,
    Previous,
    Next,
    CreatedAt,
    Error,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
}
