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
            .create_table(
                Table::create()
                    .table(Alias::new("base_worker"))
                    .if_not_exists()
                    .col(ColumnDef::new(Alias::new("id")).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Alias::new("worker_id")).string().not_null())
                    .col(ColumnDef::new(Alias::new("token_hash")).string().not_null())
                    .col(ColumnDef::new(Alias::new("url")).string().null())
                    .col(ColumnDef::new(Alias::new("display_name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("enable_fetch")).boolean().not_null().default(true))
                    .col(ColumnDef::new(Alias::new("enable_eval")).boolean().not_null().default(true))
                    .col(ColumnDef::new(Alias::new("enable_build")).boolean().not_null().default(true))
                    .col(ColumnDef::new(Alias::new("enabled")).boolean().not_null().default(true))
                    .col(ColumnDef::new(Alias::new("authorize_against")).uuid().null())
                    .col(ColumnDef::new(Alias::new("created_by")).uuid().null())
                    .col(ColumnDef::new(Alias::new("created_at")).timestamp().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .table(Alias::new("base_worker"))
                    .col(Alias::new("worker_id"))
                    .name("idx_base_worker_worker_id")
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-base_worker-created_by")
                    .from(Alias::new("base_worker"), Alias::new("created_by"))
                    .to(Alias::new("user"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("base_worker")).to_owned())
            .await
    }
}
