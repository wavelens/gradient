/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add managed field to user table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("user"))
                    .add_column(
                        ColumnDef::new(Alias::new("managed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Add managed field to organization table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("organization"))
                    .add_column(
                        ColumnDef::new(Alias::new("managed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Add managed field to project table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project"))
                    .add_column(
                        ColumnDef::new(Alias::new("managed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Add managed field to server table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("server"))
                    .add_column(
                        ColumnDef::new(Alias::new("managed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Add managed field to cache table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("cache"))
                    .add_column(
                        ColumnDef::new(Alias::new("managed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Add managed field to api table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("api"))
                    .add_column(
                        ColumnDef::new(Alias::new("managed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Remove managed field from user table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("user"))
                    .drop_column(Alias::new("managed"))
                    .to_owned(),
            )
            .await?;

        // Remove managed field from organization table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("organization"))
                    .drop_column(Alias::new("managed"))
                    .to_owned(),
            )
            .await?;

        // Remove managed field from project table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project"))
                    .drop_column(Alias::new("managed"))
                    .to_owned(),
            )
            .await?;

        // Remove managed field from server table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("server"))
                    .drop_column(Alias::new("managed"))
                    .to_owned(),
            )
            .await?;

        // Remove managed field from cache table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("cache"))
                    .drop_column(Alias::new("managed"))
                    .to_owned(),
            )
            .await?;

        // Remove managed field from api table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("api"))
                    .drop_column(Alias::new("managed"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
