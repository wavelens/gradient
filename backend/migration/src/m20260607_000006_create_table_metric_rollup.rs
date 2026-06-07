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
                    .table(MetricRollup::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(MetricRollup::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(MetricRollup::Metric).string().not_null())
                    .col(ColumnDef::new(MetricRollup::Granularity).small_integer().not_null())
                    .col(ColumnDef::new(MetricRollup::BucketStart).date_time().not_null())
                    .col(ColumnDef::new(MetricRollup::Scope).json_binary().not_null())
                    .col(ColumnDef::new(MetricRollup::ScopeHash).big_integer().not_null())
                    .col(ColumnDef::new(MetricRollup::Count).big_integer().not_null().default(0))
                    .col(ColumnDef::new(MetricRollup::Sum).double().not_null().default(0.0))
                    .col(ColumnDef::new(MetricRollup::Min).double().not_null().default(0.0))
                    .col(ColumnDef::new(MetricRollup::Max).double().not_null().default(0.0))
                    .col(ColumnDef::new(MetricRollup::SumSq).double().not_null().default(0.0))
                    .col(ColumnDef::new(MetricRollup::Histogram).json_binary().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-metric_rollup-unique")
                    .table(MetricRollup::Table)
                    .col(MetricRollup::Metric)
                    .col(MetricRollup::Granularity)
                    .col(MetricRollup::BucketStart)
                    .col(MetricRollup::ScopeHash)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-metric_rollup-query")
                    .table(MetricRollup::Table)
                    .col(MetricRollup::Metric)
                    .col(MetricRollup::Granularity)
                    .col((MetricRollup::BucketStart, IndexOrder::Desc))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MetricRollup::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum MetricRollup {
    Table,
    Id,
    Metric,
    Granularity,
    BucketStart,
    Scope,
    ScopeHash,
    Count,
    Sum,
    Min,
    Max,
    SumSq,
    Histogram,
}
