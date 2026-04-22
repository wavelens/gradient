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
                    .table(Alias::new("integration"))
                    .add_column(
                        ColumnDef::new(Alias::new("display_name"))
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;

        // Backfill display_name with the existing name so it isn't blank.
        let db = manager.get_connection();
        db.execute_unprepared(
            "UPDATE integration SET display_name = name WHERE display_name = ''",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("integration"))
                    .drop_column(Alias::new("display_name"))
                    .to_owned(),
            )
            .await
    }
}
