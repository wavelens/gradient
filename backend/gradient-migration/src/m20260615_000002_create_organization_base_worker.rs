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
                    .table(Alias::new("organization_base_worker"))
                    .if_not_exists()
                    .col(ColumnDef::new(Alias::new("id")).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Alias::new("organization")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("base_worker")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("created_by")).uuid().null())
                    .col(ColumnDef::new(Alias::new("created_at")).timestamp().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .table(Alias::new("organization_base_worker"))
                    .col(Alias::new("organization"))
                    .col(Alias::new("base_worker"))
                    .name("idx_org_base_worker_unique")
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-org_base_worker-organization")
                    .from(Alias::new("organization_base_worker"), Alias::new("organization"))
                    .to(Alias::new("organization"), Alias::new("id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-org_base_worker-base_worker")
                    .from(Alias::new("organization_base_worker"), Alias::new("base_worker"))
                    .to(Alias::new("base_worker"), Alias::new("id"))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("organization_base_worker")).to_owned())
            .await
    }
}
