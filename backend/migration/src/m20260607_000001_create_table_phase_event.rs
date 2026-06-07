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
                    .table(PhaseEvent::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(PhaseEvent::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(PhaseEvent::SubjectKind).small_integer().not_null())
                    .col(ColumnDef::new(PhaseEvent::SubjectId).uuid().not_null())
                    .col(ColumnDef::new(PhaseEvent::Phase).small_integer().not_null())
                    .col(ColumnDef::new(PhaseEvent::Event).small_integer().not_null())
                    .col(ColumnDef::new(PhaseEvent::At).date_time().not_null())
                    .col(ColumnDef::new(PhaseEvent::WorkerId).string().null())
                    .col(ColumnDef::new(PhaseEvent::Detail).json_binary().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-phase_event-subject")
                    .table(PhaseEvent::Table)
                    .col(PhaseEvent::SubjectKind)
                    .col(PhaseEvent::SubjectId)
                    .col(PhaseEvent::At)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-phase_event-phase-at")
                    .table(PhaseEvent::Table)
                    .col(PhaseEvent::Phase)
                    .col(PhaseEvent::At)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PhaseEvent::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum PhaseEvent {
    Table,
    Id,
    SubjectKind,
    SubjectId,
    Phase,
    Event,
    At,
    WorkerId,
    Detail,
}
