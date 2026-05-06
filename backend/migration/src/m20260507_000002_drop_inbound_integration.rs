/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Inbound integration linkage moves to project_trigger; drop the column on
//! project_integration. Outbound link is unchanged.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ProjectIntegration::Table)
                    .drop_column(ProjectIntegration::InboundIntegration)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ProjectIntegration::Table)
                    .add_column(ColumnDef::new(ProjectIntegration::InboundIntegration).uuid().null())
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ProjectIntegration {
    #[sea_orm(iden = "project_integration")]
    Table,
    InboundIntegration,
}
