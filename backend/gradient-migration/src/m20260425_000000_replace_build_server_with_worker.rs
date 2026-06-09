/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Replaces the dead `build.server` column (a dangling `Uuid` reference to
//! the SSH-era `server` table, dropped in
//! `m20260412_000003_drop_ssh_server_tables`) with `build.worker`, a nullable
//! text column holding the worker's `worker_id` identity string as sent in
//! `InitConnection`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_column(Build::Server)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .add_column(ColumnDef::new(Build::Worker).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_column(Build::Worker)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .add_column(ColumnDef::new(Build::Server).uuid().null())
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Server,
    Worker,
}
