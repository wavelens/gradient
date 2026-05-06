/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Add `project.sign_cache` (default `true`). When `false`, the sign sweep
//! leaves narinfo signatures NULL for paths produced solely by this project,
//! so external Nix clients won't trust them — making the project's outputs
//! private even when the cache itself is public.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .add_column(
                        ColumnDef::new(Project::SignCache)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .drop_column(Project::SignCache)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Project {
    Table,
    SignCache,
}
