/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-attempt history for outgoing webhook deliveries. Lets operators
//! diagnose 4xx/5xx from receivers and gives the UI a "last delivery"
//! summary per webhook.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(WebhookDelivery::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WebhookDelivery::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(WebhookDelivery::WebhookId).uuid().not_null())
                    .col(ColumnDef::new(WebhookDelivery::Event).string().not_null())
                    .col(
                        ColumnDef::new(WebhookDelivery::RequestBody)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDelivery::ResponseStatus)
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(WebhookDelivery::ResponseBody).text().null())
                    .col(ColumnDef::new(WebhookDelivery::ErrorMessage).text().null())
                    .col(
                        ColumnDef::new(WebhookDelivery::Success)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDelivery::DurationMs)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDelivery::DeliveredAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-webhook_delivery-webhook")
                            .from(WebhookDelivery::Table, WebhookDelivery::WebhookId)
                            .to(Webhook::Table, Webhook::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_webhook_delivery_webhook_delivered_at")
                    .table(WebhookDelivery::Table)
                    .col(WebhookDelivery::WebhookId)
                    .col(WebhookDelivery::DeliveredAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WebhookDelivery::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum WebhookDelivery {
    #[sea_orm(iden = "webhook_delivery")]
    Table,
    Id,
    WebhookId,
    Event,
    RequestBody,
    ResponseStatus,
    ResponseBody,
    ErrorMessage,
    Success,
    DurationMs,
    DeliveredAt,
}

#[derive(DeriveIden)]
enum Webhook {
    Table,
    Id,
}
