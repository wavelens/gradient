/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Storage for the OAuth 2.0 Device Authorization Grant used by `gradient login`.
//!
//! One row per `gradient login` invocation. The CLI polls `/auth/cli/poll` with
//! `device_code` (stored hashed) until the browser-side user clicks Authorize at
//! `/account/cli-authorize?code=<user_code>`, at which point `user_id` and
//! `token` are populated and subsequent polls return the token.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CliDeviceAuthorization::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::DeviceCodeHash)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::UserCode)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(CliDeviceAuthorization::UserId).uuid().null())
                    .col(ColumnDef::new(CliDeviceAuthorization::Token).text().null())
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::DeniedAt)
                            .date_time()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::AuthorizedAt)
                            .date_time()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::ExpiresAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CliDeviceAuthorization::UserAgent)
                            .text()
                            .null(),
                    )
                    .col(ColumnDef::new(CliDeviceAuthorization::Ip).string().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cli-device-auth-user")
                            .from(
                                CliDeviceAuthorization::Table,
                                CliDeviceAuthorization::UserId,
                            )
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_cli_device_auth_expires_at")
                    .table(CliDeviceAuthorization::Table)
                    .col(CliDeviceAuthorization::ExpiresAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(CliDeviceAuthorization::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum CliDeviceAuthorization {
    Table,
    Id,
    DeviceCodeHash,
    UserCode,
    UserId,
    Token,
    DeniedAt,
    AuthorizedAt,
    CreatedAt,
    ExpiresAt,
    UserAgent,
    Ip,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
