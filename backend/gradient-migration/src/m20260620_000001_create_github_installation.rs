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
                    .table(Alias::new("github_installation"))
                    .if_not_exists()
                    .col(ColumnDef::new(Alias::new("id")).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Alias::new("organization")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("installation_id")).big_integer().not_null())
                    .col(ColumnDef::new(Alias::new("account_login")).text().null())
                    .col(ColumnDef::new(Alias::new("created_by")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("created_at")).timestamp().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-github_installation-organization")
                            .from(Alias::new("github_installation"), Alias::new("organization"))
                            .to(Alias::new("organization"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-github_installation-created_by")
                            .from(Alias::new("github_installation"), Alias::new("created_by"))
                            .to(Alias::new("user"), Alias::new("id")),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-github-installation-org-installation")
                    .table(Alias::new("github_installation"))
                    .col(Alias::new("organization"))
                    .col(Alias::new("installation_id"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("integration"))
                    .add_column(ColumnDef::new(Alias::new("github_installation")).uuid().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-integration-github-installation")
                    .from(Alias::new("integration"), Alias::new("github_installation"))
                    .to(Alias::new("github_installation"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk-integration-github-installation")
                    .table(Alias::new("integration"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("integration"))
                    .drop_column(Alias::new("github_installation"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("github_installation")).to_owned())
            .await
    }
}
