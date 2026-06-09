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
                    .table(Alias::new("evaluation"))
                    .add_column(
                        ColumnDef::new(Alias::new("updated_at"))
                            .date_time()
                            .not_null()
                            .default(SimpleExpr::Custom("NOW()".to_owned())),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("evaluation"))
                    .drop_column(Alias::new("updated_at"))
                    .to_owned(),
            )
            .await
    }
}
