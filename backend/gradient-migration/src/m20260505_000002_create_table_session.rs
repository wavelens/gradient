/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds the `session` table that backs JWT revocation.
//!
//! Each issued JWT carries a `jti` matching a row in this table. The auth
//! middleware rejects tokens whose session row is missing, has `revoked_at`
//! set, or is past `expires_at`. This is what makes logout effective and
//! enables the "logged-in devices" UI.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Session::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Session::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Session::UserId).uuid().not_null())
                    .col(ColumnDef::new(Session::CreatedAt).date_time().not_null())
                    .col(ColumnDef::new(Session::ExpiresAt).date_time().not_null())
                    .col(ColumnDef::new(Session::LastUsedAt).date_time().not_null())
                    .col(ColumnDef::new(Session::RevokedAt).date_time().null())
                    .col(ColumnDef::new(Session::UserAgent).text().null())
                    .col(ColumnDef::new(Session::Ip).string().null())
                    .col(
                        ColumnDef::new(Session::RememberMe)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-session-user")
                            .from(Session::Table, Session::UserId)
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
                    .name("idx_session_user_id")
                    .table(Session::Table)
                    .col(Session::UserId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Session::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Session {
    Table,
    Id,
    UserId,
    CreatedAt,
    ExpiresAt,
    LastUsedAt,
    RevokedAt,
    UserAgent,
    Ip,
    RememberMe,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
