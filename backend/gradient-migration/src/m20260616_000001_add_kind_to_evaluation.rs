/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Distinguishes `normal` evaluations from `input_update` runs (kind = 1).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .add_column(
                        ColumnDef::new(Evaluation::Kind).integer().not_null().default(0),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .drop_column(Evaluation::Kind)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Kind,
}
