/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Records which `project_trigger` row created an evaluation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .add_column(ColumnDef::new(Evaluation::Trigger).uuid().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-evaluation-trigger")
                    .from(Evaluation::Table, Evaluation::Trigger)
                    .to(ProjectTrigger::Table, ProjectTrigger::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .on_update(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk-evaluation-trigger")
                    .table(Evaluation::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .drop_column(Evaluation::Trigger)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Trigger,
}

#[derive(DeriveIden)]
enum ProjectTrigger {
    #[sea_orm(iden = "project_trigger")]
    Table,
    Id,
}
