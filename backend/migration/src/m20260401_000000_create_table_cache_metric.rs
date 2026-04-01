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
            .create_table(
                Table::create()
                    .table(CacheMetric::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(CacheMetric::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(CacheMetric::Cache).uuid().not_null())
                    .col(ColumnDef::new(CacheMetric::BucketTime).date_time().not_null())
                    .col(
                        ColumnDef::new(CacheMetric::BytesSent)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(CacheMetric::NarCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_metric-cache")
                            .from(CacheMetric::Table, CacheMetric::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-cache_metric-cache-bucket_time")
                    .table(CacheMetric::Table)
                    .col(CacheMetric::Cache)
                    .col(CacheMetric::BucketTime)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CacheMetric::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CacheMetric {
    Table,
    Id,
    Cache,
    BucketTime,
    BytesSent,
    NarCount,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}
