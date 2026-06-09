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
            .alter_table(
                Table::alter()
                    .table(DerivationMetric::Table)
                    .add_column(ColumnDef::new(DerivationMetric::PeakNetworkMbps).double().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DerivationMetric::Table)
                    .drop_column(DerivationMetric::PeakNetworkMbps)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum DerivationMetric {
    Table,
    PeakNetworkMbps,
}
