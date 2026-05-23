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
                    .table(ProjectAction::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectAction::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ProjectAction::Project).uuid().not_null())
                    .col(ColumnDef::new(ProjectAction::Name).string().not_null())
                    .col(
                        ColumnDef::new(ProjectAction::ActionType)
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectAction::Config)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectAction::Events)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectAction::Active)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ProjectAction::LastFiredAt)
                            .date_time()
                            .null(),
                    )
                    .col(ColumnDef::new(ProjectAction::CreatedBy).uuid().not_null())
                    .col(
                        ColumnDef::new(ProjectAction::CreatedAt)
                            .date_time()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ProjectAction::UpdatedAt)
                            .date_time()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_action-project")
                            .from(ProjectAction::Table, ProjectAction::Project)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_action-created_by")
                            .from(ProjectAction::Table, ProjectAction::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_project_action_project_name")
                    .table(ProjectAction::Table)
                    .col(ProjectAction::Project)
                    .col(ProjectAction::Name)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ProjectAction::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ProjectAction {
    #[sea_orm(iden = "project_action")]
    Table,
    Id,
    Project,
    Name,
    ActionType,
    Config,
    Events,
    Active,
    LastFiredAt,
    CreatedBy,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
