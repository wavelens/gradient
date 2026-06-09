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
        for col in ["enable_fetch", "enable_eval", "enable_build"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("worker_registration"))
                        .add_column(
                            ColumnDef::new(Alias::new(col))
                                .boolean()
                                .not_null()
                                .default(true),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for col in ["enable_build", "enable_eval", "enable_fetch"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("worker_registration"))
                        .drop_column(Alias::new(col))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
