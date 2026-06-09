/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Drop the legacy `project_integration` link table. The per-project outbound
//! integration link is replaced by `ForgeStatusReport` actions that reference
//! org-level integrations directly (issue #262).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ProjectIntegration::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "drop_table_project_integration is irreversible: project_integration removed in issue #262"
                .into(),
        ))
    }
}

#[derive(DeriveIden)]
enum ProjectIntegration {
    #[sea_orm(iden = "project_integration")]
    Table,
}
