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
                    .table(WorkerSample::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(WorkerSample::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(WorkerSample::WorkerId).string().not_null())
                    .col(ColumnDef::new(WorkerSample::Organization).uuid().not_null())
                    .col(ColumnDef::new(WorkerSample::At).date_time().not_null())
                    .col(ColumnDef::new(WorkerSample::CpuUsagePct).float().null())
                    .col(ColumnDef::new(WorkerSample::RamFreeMb).big_integer().null())
                    .col(ColumnDef::new(WorkerSample::RamTotalMb).big_integer().null())
                    .col(ColumnDef::new(WorkerSample::DiskSpeedMbps).float().null())
                    .col(ColumnDef::new(WorkerSample::NetworkSpeedMbps).float().null())
                    .col(ColumnDef::new(WorkerSample::AssignedJobs).integer().not_null().default(0))
                    .col(ColumnDef::new(WorkerSample::MaxConcurrentBuilds).integer().not_null().default(0))
                    .col(ColumnDef::new(WorkerSample::State).small_integer().not_null().default(0))
                    .col(ColumnDef::new(WorkerSample::Capabilities).json_binary().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-worker_sample-worker-at")
                    .table(WorkerSample::Table)
                    .col(WorkerSample::WorkerId)
                    .col((WorkerSample::At, IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-worker_sample-at")
                    .table(WorkerSample::Table)
                    .col((WorkerSample::At, IndexOrder::Desc))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WorkerSample::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum WorkerSample {
    Table,
    Id,
    WorkerId,
    Organization,
    At,
    CpuUsagePct,
    RamFreeMb,
    RamTotalMb,
    DiskSpeedMbps,
    NetworkSpeedMbps,
    AssignedJobs,
    MaxConcurrentBuilds,
    State,
    Capabilities,
}
