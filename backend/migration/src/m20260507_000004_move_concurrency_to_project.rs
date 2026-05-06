/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Move concurrency policy from per-trigger to per-project. Adds
//! `project.concurrency` (default 3 = skip) and drops the column on
//! `project_trigger`. No data migration — every project starts at `skip`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .add_column(
                        ColumnDef::new(Project::Concurrency)
                            .small_integer()
                            .not_null()
                            .default(3),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ProjectTrigger::Table)
                    .drop_column(ProjectTrigger::Concurrency)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ProjectTrigger::Table)
                    .add_column(
                        ColumnDef::new(ProjectTrigger::Concurrency)
                            .small_integer()
                            .not_null()
                            .default(3),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .drop_column(Project::Concurrency)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Concurrency,
}

#[derive(DeriveIden)]
enum ProjectTrigger {
    #[sea_orm(iden = "project_trigger")]
    Table,
    Concurrency,
}
