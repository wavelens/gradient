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
                    .table(Derivation::Table)
                    .add_column(ColumnDef::new(Derivation::Pname).string().null())
                    .add_column(
                        ColumnDef::new(Derivation::PreferLocalBuild)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .add_column(
                        ColumnDef::new(Derivation::AllowSubstitutes)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .add_column(ColumnDef::new(Derivation::ClosureSize).big_integer().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .drop_column(Derivation::ClosureSize)
                    .drop_column(Derivation::AllowSubstitutes)
                    .drop_column(Derivation::PreferLocalBuild)
                    .drop_column(Derivation::Pname)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Derivation {
    Table,
    Pname,
    PreferLocalBuild,
    AllowSubstitutes,
    ClosureSize,
}
