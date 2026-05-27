/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nullable `allowed_ips TEXT[]` on `integration`; NULL/empty = allow any source.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Integration::Table)
                    .add_column(
                        ColumnDef::new(Integration::AllowedIps)
                            .array(ColumnType::Text)
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
                    .table(Integration::Table)
                    .drop_column(Integration::AllowedIps)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Integration {
    Table,
    AllowedIps,
}
