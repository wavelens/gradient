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
        // ── 1. integration table ─────────────────────────────────────────────
        // kind:       0 = inbound, 1 = outbound
        // forge_type: 0 = gitea, 1 = forgejo, 2 = gitlab, 3 = github
        manager
            .create_table(
                Table::create()
                    .table(Integration::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Integration::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Integration::Organization).uuid().not_null())
                    .col(ColumnDef::new(Integration::Name).string().not_null())
                    .col(ColumnDef::new(Integration::Kind).small_integer().not_null())
                    .col(
                        ColumnDef::new(Integration::ForgeType)
                            .small_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Integration::Secret).text())
                    .col(ColumnDef::new(Integration::EndpointUrl).text())
                    .col(ColumnDef::new(Integration::AccessToken).text())
                    .col(ColumnDef::new(Integration::CreatedBy).uuid().not_null())
                    .col(
                        ColumnDef::new(Integration::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-integration-organization")
                            .from(Integration::Table, Integration::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-integration-created_by")
                            .from(Integration::Table, Integration::CreatedBy)
                            .to(User::Table, User::Id),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-integration-org-kind-name")
                    .table(Integration::Table)
                    .col(Integration::Organization)
                    .col(Integration::Kind)
                    .col(Integration::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ── 2. project_integration table ─────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(ProjectIntegration::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectIntegration::Project)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ProjectIntegration::InboundIntegration).uuid())
                    .col(ColumnDef::new(ProjectIntegration::OutboundIntegration).uuid())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_integration-project")
                            .from(ProjectIntegration::Table, ProjectIntegration::Project)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_integration-inbound")
                            .from(
                                ProjectIntegration::Table,
                                ProjectIntegration::InboundIntegration,
                            )
                            .to(Integration::Table, Integration::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_integration-outbound")
                            .from(
                                ProjectIntegration::Table,
                                ProjectIntegration::OutboundIntegration,
                            )
                            .to(Integration::Table, Integration::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // ── 3. Drop legacy columns ───────────────────────────────────────────
        manager
            .alter_table(
                Table::alter()
                    .table(Organization::Table)
                    .drop_column(Organization::ForgeWebhookSecret)
                    .to_owned(),
            )
            .await?;

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

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .add_column(ColumnDef::new(Project::CiReporterType).text())
                    .add_column(ColumnDef::new(Project::CiReporterUrl).text())
                    .add_column(ColumnDef::new(Project::CiReporterToken).text())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Organization::Table)
                    .add_column(ColumnDef::new(Organization::ForgeWebhookSecret).text())
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(ProjectIntegration::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Integration::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Integration {
    Table,
    Id,
    Organization,
    Name,
    Kind,
    ForgeType,
    Secret,
    EndpointUrl,
    AccessToken,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ProjectIntegration {
    Table,
    Project,
    InboundIntegration,
    OutboundIntegration,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
    ForgeWebhookSecret,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
    CiReporterType,
    CiReporterUrl,
    CiReporterToken,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
