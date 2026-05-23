/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Drop the legacy `webhook` table. Replaced by per-project Actions
//! (issue #262); the table is unreferenced after webhook_delivery is gone.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Webhook::Table).if_exists().to_owned())
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "drop_table_webhook is irreversible: webhook removed in issue #262".into(),
        ))
    }
}

#[derive(DeriveIden)]
enum Webhook {
    #[sea_orm(iden = "webhook")]
    Table,
}
