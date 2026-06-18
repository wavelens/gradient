/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Drop the redundant `cached_path.store_path` column. The full path is fully
//! determined by `hash` + `package` and is reconstructed on read via
//! `cached_path::Model::store_path()`.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CachedPath::Table)
                    .drop_column(CachedPath::StorePath)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CachedPath::Table)
                    .add_column(
                        ColumnDef::new(CachedPath::StorePath)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE cached_path SET store_path = '/nix/store/' || hash || '-' || package",
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum CachedPath {
    Table,
    StorePath,
}
