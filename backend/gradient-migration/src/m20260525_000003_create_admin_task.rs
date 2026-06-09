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
                    .table(AdminTask::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AdminTask::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AdminTask::Kind).integer().not_null())
                    .col(ColumnDef::new(AdminTask::Status).integer().not_null())
                    .col(
                        ColumnDef::new(AdminTask::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(AdminTask::StartedAt).timestamp().null())
                    .col(ColumnDef::new(AdminTask::FinishedAt).timestamp().null())
                    .col(ColumnDef::new(AdminTask::Progress).json_binary().null())
                    .col(ColumnDef::new(AdminTask::Error).text().null())
                    .col(ColumnDef::new(AdminTask::CreatedBy).uuid().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-admin_task-created_by")
                            .from(AdminTask::Table, AdminTask::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE UNIQUE INDEX admin_task_one_active_per_kind
                   ON admin_task (kind)
                   WHERE status IN (0, 1)"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS admin_task_one_active_per_kind")
            .await?;
        manager
            .drop_table(Table::drop().table(AdminTask::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AdminTask {
    Table,
    Id,
    Kind,
    Status,
    CreatedAt,
    StartedAt,
    FinishedAt,
    Progress,
    Error,
    CreatedBy,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
