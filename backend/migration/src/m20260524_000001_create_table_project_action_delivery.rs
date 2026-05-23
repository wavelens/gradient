/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Create the project_action_delivery history table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ProjectActionDelivery::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectActionDelivery::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::ActionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::Event)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::RequestBody)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::ResponseStatus)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::ResponseBody)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::ErrorMessage)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::Success)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::DurationMs)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectActionDelivery::DeliveredAt)
                            .date_time()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_action_delivery-action_id")
                            .from(
                                ProjectActionDelivery::Table,
                                ProjectActionDelivery::ActionId,
                            )
                            .to(ProjectAction::Table, ProjectAction::Id)
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
                    .name("idx_project_action_delivery_action_delivered")
                    .table(ProjectActionDelivery::Table)
                    .col(ProjectActionDelivery::ActionId)
                    .col((ProjectActionDelivery::DeliveredAt, IndexOrder::Desc))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ProjectActionDelivery::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ProjectActionDelivery {
    #[sea_orm(iden = "project_action_delivery")]
    Table,
    Id,
    ActionId,
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
enum ProjectAction {
    #[sea_orm(iden = "project_action")]
    Table,
    Id,
}
