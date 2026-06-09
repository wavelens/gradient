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
                    .table(Alias::new("worker_registration"))
                    .rename_column(Alias::new("name"), Alias::new("display_name"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("worker_registration"))
                    .add_column(ColumnDef::new(Alias::new("created_by")).uuid().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-worker_registration-created_by")
                    .from(Alias::new("worker_registration"), Alias::new("created_by"))
                    .to(Alias::new("user"), Alias::new("id"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk-worker_registration-created_by")
                    .table(Alias::new("worker_registration"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("worker_registration"))
                    .drop_column(Alias::new("created_by"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("worker_registration"))
                    .rename_column(Alias::new("display_name"), Alias::new("name"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
