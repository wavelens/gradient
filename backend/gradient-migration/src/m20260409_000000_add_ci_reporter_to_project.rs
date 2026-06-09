/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260409_000000_add_ci_reporter_to_project"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .add_column(ColumnDef::new(Project::CiReporterType).string().null())
                    .add_column(ColumnDef::new(Project::CiReporterUrl).string().null())
                    .add_column(ColumnDef::new(Project::CiReporterToken).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Organization::Table)
                    .add_column(
                        ColumnDef::new(Organization::GithubInstallationId)
                            .big_integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(Organization::ForgeWebhookSecret)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .drop_column(Project::CiReporterType)
                    .drop_column(Project::CiReporterUrl)
                    .drop_column(Project::CiReporterToken)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Organization::Table)
                    .drop_column(Organization::GithubInstallationId)
                    .drop_column(Organization::ForgeWebhookSecret)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Project {
    Table,
    CiReporterType,
    CiReporterUrl,
    CiReporterToken,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    GithubInstallationId,
    ForgeWebhookSecret,
}
