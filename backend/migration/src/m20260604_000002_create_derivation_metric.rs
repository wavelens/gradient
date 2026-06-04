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
                    .table(DerivationMetric::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DerivationMetric::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DerivationMetric::Derivation)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DerivationMetric::Pname).string().null())
                    .col(ColumnDef::new(DerivationMetric::ClosureSize).big_integer().null())
                    .col(ColumnDef::new(DerivationMetric::PeakRamMb).big_integer().null())
                    .col(ColumnDef::new(DerivationMetric::CpuTimeMs).big_integer().null())
                    .col(ColumnDef::new(DerivationMetric::AvgCpuPct).double().null())
                    .col(ColumnDef::new(DerivationMetric::DiskReadBytes).big_integer().null())
                    .col(ColumnDef::new(DerivationMetric::DiskWriteBytes).big_integer().null())
                    .col(
                        ColumnDef::new(DerivationMetric::OomKilled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(DerivationMetric::BuildTimeMs).big_integer().null())
                    .col(ColumnDef::new(DerivationMetric::WorkerId).string().not_null())
                    .col(
                        ColumnDef::new(DerivationMetric::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_metric-derivation")
                            .from(DerivationMetric::Table, DerivationMetric::Derivation)
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-derivation_metric-pname-closure_size")
                    .table(DerivationMetric::Table)
                    .col(DerivationMetric::Pname)
                    .col(DerivationMetric::ClosureSize)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DerivationMetric::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DerivationMetric {
    Table,
    Id,
    Derivation,
    Pname,
    ClosureSize,
    PeakRamMb,
    CpuTimeMs,
    AvgCpuPct,
    DiskReadBytes,
    DiskWriteBytes,
    OomKilled,
    BuildTimeMs,
    WorkerId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Derivation {
    Table,
    Id,
}
