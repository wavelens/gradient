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
                    .table(WorkerConnection::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(WorkerConnection::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(WorkerConnection::WorkerId).string().not_null())
                    .col(ColumnDef::new(WorkerConnection::Organization).uuid().not_null())
                    .col(ColumnDef::new(WorkerConnection::DisplayName).string().not_null())
                    .col(ColumnDef::new(WorkerConnection::ConnectedAt).date_time().not_null())
                    .col(ColumnDef::new(WorkerConnection::DisconnectedAt).date_time().null())
                    .col(ColumnDef::new(WorkerConnection::Capabilities).json_binary().not_null())
                    .col(ColumnDef::new(WorkerConnection::Reason).small_integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-worker_connection-worker-connected_at")
                    .table(WorkerConnection::Table)
                    .col(WorkerConnection::WorkerId)
                    .col((WorkerConnection::ConnectedAt, IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS \"idx-worker_connection-open\" \
                 ON \"worker_connection\" (\"connected_at\" DESC) \
                 WHERE \"disconnected_at\" IS NULL",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WorkerConnection::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum WorkerConnection {
    Table,
    Id,
    WorkerId,
    Organization,
    DisplayName,
    ConnectedAt,
    DisconnectedAt,
    Capabilities,
    Reason,
}
