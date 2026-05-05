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
                    .table(Api::Table)
                    .add_column(ColumnDef::new(Api::ExpiresAt).date_time().null())
                    .add_column(ColumnDef::new(Api::RevokedAt).date_time().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_api_owned_by")
                    .table(Api::Table)
                    .col(Api::OwnedBy)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_api_owned_by")
                    .table(Api::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Api::Table)
                    .drop_column(Api::ExpiresAt)
                    .drop_column(Api::RevokedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Api {
    Table,
    OwnedBy,
    ExpiresAt,
    RevokedAt,
}
