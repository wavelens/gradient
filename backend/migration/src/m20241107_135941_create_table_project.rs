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
                    .table(Project::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Project::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Project::Organization).uuid().not_null())
                    .col(ColumnDef::new(Project::Name).string().not_null())
                    .col(ColumnDef::new(Project::Enabled).boolean().not_null())
                    .col(ColumnDef::new(Project::DisplayName).string().not_null())
                    .col(ColumnDef::new(Project::Description).text().not_null())
                    .col(ColumnDef::new(Project::Repository).string().not_null())
                    .col(
                        ColumnDef::new(Project::EvaluationWildcard)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Project::LastEvaluation).uuid())
                    .col(ColumnDef::new(Project::LastCheckAt).date_time().not_null())
                    .col(
                        ColumnDef::new(Project::ForceEvaluation)
                            .boolean()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Project::CreatedBy).uuid().not_null())
                    .col(ColumnDef::new(Project::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project-organization")
                            .from(Project::Table, Project::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project-created_by")
                            .from(Project::Table, Project::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Project::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
    Organization,
    Name,
    Enabled,
    DisplayName,
    Description,
    Repository,
    EvaluationWildcard,
    LastEvaluation,
    LastCheckAt,
    ForceEvaluation,
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
