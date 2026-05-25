/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds an optional `cache` pin to API keys. Mutually exclusive with the
//! existing `organization` pin (enforced at the API layer, not the schema).

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
                    .add_column(ColumnDef::new(Api::Cache).uuid().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-api-cache")
                    .from(Api::Table, Api::Cache)
                    .to(Cache::Table, Cache::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .on_update(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_api_cache")
                    .table(Api::Table)
                    .col(Api::Cache)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_api_cache")
                    .table(Api::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk-api-cache")
                    .table(Api::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Api::Table)
                    .drop_column(Api::Cache)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Api {
    Table,
    Cache,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}
