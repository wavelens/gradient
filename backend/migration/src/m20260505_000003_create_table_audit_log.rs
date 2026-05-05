/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Append-only audit log for security-relevant events (auth, key lifecycle,
//! account deletion). `user_id` is nullable so that anonymous-context events
//! (e.g. failed login attempts) can still be recorded.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AuditLog::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(AuditLog::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(AuditLog::UserId).uuid().null())
                    .col(ColumnDef::new(AuditLog::Event).string().not_null())
                    .col(ColumnDef::new(AuditLog::Ip).string().null())
                    .col(ColumnDef::new(AuditLog::UserAgent).text().null())
                    .col(ColumnDef::new(AuditLog::Metadata).json().null())
                    .col(ColumnDef::new(AuditLog::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-audit_log-user")
                            .from(AuditLog::Table, AuditLog::UserId)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_audit_log_user_id_created_at")
                    .table(AuditLog::Table)
                    .col(AuditLog::UserId)
                    .col(AuditLog::CreatedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AuditLog::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AuditLog {
    #[sea_orm(iden = "audit_log")]
    Table,
    Id,
    UserId,
    Event,
    Ip,
    UserAgent,
    Metadata,
    CreatedAt,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
