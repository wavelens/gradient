/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Drops `derivation_output.file_hash` and `derivation_output.file_size`. The
//! authoritative copies live on `cached_path`; the columns on
//! `derivation_output` were write-only mirrors that the worker handler stopped
//! filling consistently, so cache reads were forced to fall back to
//! `cached_path` anyway.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DerivationOutput::Table)
                    .drop_column(DerivationOutput::FileHash)
                    .drop_column(DerivationOutput::FileSize)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DerivationOutput::Table)
                    .add_column(ColumnDef::new(DerivationOutput::FileHash).text().null())
                    .add_column(
                        ColumnDef::new(DerivationOutput::FileSize)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum DerivationOutput {
    Table,
    FileHash,
    FileSize,
}
