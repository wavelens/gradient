/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds `build.external_cached`. When true, the build's outputs are known to
//! be available from an upstream cache (cache.nixos.org etc.) but are NOT
//! yet in the gradient cache. The dispatcher hands these jobs to a worker
//! which downloads from upstream, recompresses, and pushes to our cache -
//! it does not actually rebuild from source. When false, the build is
//! either Substituted (status enum) or a real `nix build`.
//!
//! Defaulted to `false`; existing rows preserve current behavior.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .add_column(
                        ColumnDef::new(Build::ExternalCached)
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
                    .table(Build::Table)
                    .drop_column(Build::ExternalCached)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Build {
    Table,
    ExternalCached,
}
