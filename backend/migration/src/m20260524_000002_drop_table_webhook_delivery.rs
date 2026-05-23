/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Drop the legacy `webhook_delivery` table. The per-org webhook surface has
//! been superseded by per-project Actions (issue #262), whose delivery audit
//! lives in `project_action_delivery`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(WebhookDelivery::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "drop_table_webhook_delivery is irreversible: webhook_delivery removed in issue #262"
                .into(),
        ))
    }
}

#[derive(DeriveIden)]
enum WebhookDelivery {
    #[sea_orm(iden = "webhook_delivery")]
    Table,
}
