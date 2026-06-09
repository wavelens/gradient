/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nullable `allowed_ips TEXT[]` on `api`; NULL/empty = allow any source.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Api::Table)
                    .add_column(
                        ColumnDef::new(Api::AllowedIps)
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
                    .table(Api::Table)
                    .drop_column(Api::AllowedIps)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Api {
    Table,
    AllowedIps,
}
