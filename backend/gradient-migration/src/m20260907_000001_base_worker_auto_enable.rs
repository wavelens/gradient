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
            .alter_table(
                Table::alter()
                    .table(BaseWorker::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(BaseWorker::AutoEnable)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(BaseWorker::Table)
                    .drop_column(BaseWorker::AutoEnable)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum BaseWorker {
    Table,
    AutoEnable,
}
