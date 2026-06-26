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
                    .table(UpstreamMetric::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(UpstreamMetric::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(UpstreamMetric::Upstream).uuid().not_null())
                    .col(
                        ColumnDef::new(UpstreamMetric::BucketTime)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UpstreamMetric::LatencyMsSum)
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(UpstreamMetric::RequestCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(UpstreamMetric::NarinfoHits)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(UpstreamMetric::NarinfoMisses)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-upstream_metric-upstream")
                            .from(UpstreamMetric::Table, UpstreamMetric::Upstream)
                            .to(CacheUpstream::Table, CacheUpstream::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-upstream_metric-upstream-bucket_time")
                    .table(UpstreamMetric::Table)
                    .col(UpstreamMetric::Upstream)
                    .col(UpstreamMetric::BucketTime)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UpstreamMetric::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum UpstreamMetric {
    Table,
    Id,
    Upstream,
    BucketTime,
    LatencyMsSum,
    RequestCount,
    NarinfoHits,
    NarinfoMisses,
}

#[derive(DeriveIden)]
enum CacheUpstream {
    Table,
    Id,
}
