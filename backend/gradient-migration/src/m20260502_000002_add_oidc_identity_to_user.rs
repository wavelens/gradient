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
                    .table(Alias::new("user"))
                    .add_column(ColumnDef::new(Alias::new("oidc_issuer")).string().null())
                    .add_column(ColumnDef::new(Alias::new("oidc_subject")).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_user_oidc_identity")
                    .table(Alias::new("user"))
                    .col(Alias::new("oidc_issuer"))
                    .col(Alias::new("oidc_subject"))
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_user_oidc_identity")
                    .table(Alias::new("user"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("user"))
                    .drop_column(Alias::new("oidc_issuer"))
                    .drop_column(Alias::new("oidc_subject"))
                    .to_owned(),
            )
            .await
    }
}
