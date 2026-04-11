/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Rename the `server` / `server_architecture` / `server_feature` tables to
//! `build_machine` / `build_machine_architecture` / `build_machine_feature`.
//!
//! The `server` concept conflicted with "the Gradient server" itself, so the
//! entity has been renamed to `build_machine` (a host delegated builds over SSH).
//!
//! Data is preserved — this is a pure rename with no schema changes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Rename tables.
        manager
            .rename_table(
                Table::rename()
                    .table(Alias::new("server"), Alias::new("build_machine"))
                    .to_owned(),
            )
            .await?;

        manager
            .rename_table(
                Table::rename()
                    .table(
                        Alias::new("server_architecture"),
                        Alias::new("build_machine_architecture"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .rename_table(
                Table::rename()
                    .table(
                        Alias::new("server_feature"),
                        Alias::new("build_machine_feature"),
                    )
                    .to_owned(),
            )
            .await?;

        // Rename the FK column on build_machine_architecture.
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("build_machine_architecture"))
                    .rename_column(Alias::new("server"), Alias::new("build_machine"))
                    .to_owned(),
            )
            .await?;

        // Rename the FK column on build_machine_feature.
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("build_machine_feature"))
                    .rename_column(Alias::new("server"), Alias::new("build_machine"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Rename columns back first.
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("build_machine_architecture"))
                    .rename_column(Alias::new("build_machine"), Alias::new("server"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("build_machine_feature"))
                    .rename_column(Alias::new("build_machine"), Alias::new("server"))
                    .to_owned(),
            )
            .await?;

        // Rename tables back.
        manager
            .rename_table(
                Table::rename()
                    .table(Alias::new("build_machine"), Alias::new("server"))
                    .to_owned(),
            )
            .await?;

        manager
            .rename_table(
                Table::rename()
                    .table(
                        Alias::new("build_machine_architecture"),
                        Alias::new("server_architecture"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .rename_table(
                Table::rename()
                    .table(
                        Alias::new("build_machine_feature"),
                        Alias::new("server_feature"),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
