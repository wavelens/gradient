/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260408_000001_evaluation_messages"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Drop evaluation.error — replaced by evaluation_message rows.
        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .drop_column(Evaluation::Error)
                    .to_owned(),
            )
            .await?;

        // 2. Create evaluation_message table.
        manager
            .create_table(
                Table::create()
                    .table(EvaluationMessage::Table)
                    .col(
                        ColumnDef::new(EvaluationMessage::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EvaluationMessage::Evaluation)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EvaluationMessage::Level)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(EvaluationMessage::Message).text().not_null())
                    .col(ColumnDef::new(EvaluationMessage::Source).string())
                    .col(
                        ColumnDef::new(EvaluationMessage::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-evaluation_message-evaluation")
                            .from(EvaluationMessage::Table, EvaluationMessage::Evaluation)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Index to speed up "fetch all messages for an evaluation" queries.
        manager
            .create_index(
                Index::create()
                    .name("idx-evaluation_message-evaluation")
                    .table(EvaluationMessage::Table)
                    .col(EvaluationMessage::Evaluation)
                    .to_owned(),
            )
            .await?;

        // 3. Create entry_point_message join table.
        manager
            .create_table(
                Table::create()
                    .table(EntryPointMessage::Table)
                    .col(
                        ColumnDef::new(EntryPointMessage::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EntryPointMessage::EntryPoint)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(EntryPointMessage::Message).uuid().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-entry_point_message-entry_point")
                            .from(EntryPointMessage::Table, EntryPointMessage::EntryPoint)
                            .to(EntryPoint::Table, EntryPoint::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-entry_point_message-message")
                            .from(EntryPointMessage::Table, EntryPointMessage::Message)
                            .to(EvaluationMessage::Table, EvaluationMessage::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-entry_point_message-unique")
                    .table(EntryPointMessage::Table)
                    .col(EntryPointMessage::EntryPoint)
                    .col(EntryPointMessage::Message)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EntryPointMessage::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(EvaluationMessage::Table).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .add_column(ColumnDef::new(Evaluation::Error).string())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
    Error,
}

#[derive(DeriveIden)]
#[allow(clippy::enum_variant_names)]
enum EvaluationMessage {
    Table,
    Id,
    Evaluation,
    Level,
    Message,
    Source,
    CreatedAt,
}

#[derive(DeriveIden)]
enum EntryPoint {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum EntryPointMessage {
    Table,
    Id,
    EntryPoint,
    Message,
}
