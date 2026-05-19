/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `upload_session` tracks an in-progress `gradient build` upload between
//! the client's manifest submission and dispatch. The `manifest` JSONB
//! holds the full `[(path, hash, size)]` list; `missing` is the subset of
//! BLAKE3 hex hashes the client still has to upload. A session self-expires
//! after one hour; the GC sweep drops stale undispatched sessions.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(UploadSession::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(UploadSession::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(UploadSession::Organization).uuid().not_null())
                    .col(ColumnDef::new(UploadSession::Manifest).json_binary().not_null())
                    .col(ColumnDef::new(UploadSession::Missing).json_binary().not_null())
                    .col(ColumnDef::new(UploadSession::TotalSize).big_integer().not_null())
                    .col(ColumnDef::new(UploadSession::CreatedAt).date_time().not_null())
                    .col(ColumnDef::new(UploadSession::ExpiresAt).date_time().not_null())
                    .col(ColumnDef::new(UploadSession::DispatchedAt).date_time().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-upload_session-organization")
                            .from(UploadSession::Table, UploadSession::Organization)
                            .to(Alias::new("organization"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UploadSession::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum UploadSession {
    Table,
    Id,
    Organization,
    Manifest,
    Missing,
    TotalSize,
    CreatedAt,
    ExpiresAt,
    DispatchedAt,
}
