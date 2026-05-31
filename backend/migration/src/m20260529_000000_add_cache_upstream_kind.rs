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
                    .table(CacheUpstream::Table)
                    .add_column(
                        ColumnDef::new(CacheUpstream::Kind)
                            .integer()
                            .not_null()
                            .default(2),
                    )
                    .add_column(ColumnDef::new(CacheUpstream::RemoteCacheName).text().null())
                    .add_column(ColumnDef::new(CacheUpstream::ApiKey).text().null())
                    .to_owned(),
            )
            .await?;

        let db = manager.get_connection();
        db.execute_unprepared(
            "UPDATE cache_upstream SET kind = 0 WHERE upstream_cache IS NOT NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CacheUpstream::Table)
                    .drop_column(CacheUpstream::ApiKey)
                    .drop_column(CacheUpstream::RemoteCacheName)
                    .drop_column(CacheUpstream::Kind)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum CacheUpstream {
    Table,
    Kind,
    RemoteCacheName,
    ApiKey,
}
