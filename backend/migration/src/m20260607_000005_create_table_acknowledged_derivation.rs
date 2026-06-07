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
                    .table(AcknowledgedDerivation::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(AcknowledgedDerivation::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(AcknowledgedDerivation::Derivation).uuid().null())
                    .col(ColumnDef::new(AcknowledgedDerivation::Pname).string().null())
                    .col(ColumnDef::new(AcknowledgedDerivation::Note).text().not_null())
                    .col(ColumnDef::new(AcknowledgedDerivation::CreatedBy).uuid().not_null())
                    .col(ColumnDef::new(AcknowledgedDerivation::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-acknowledged_derivation-derivation")
                            .from(AcknowledgedDerivation::Table, AcknowledgedDerivation::Derivation)
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-acknowledged_derivation-created_by")
                            .from(AcknowledgedDerivation::Table, AcknowledgedDerivation::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-acknowledged_derivation-pname")
                    .table(AcknowledgedDerivation::Table)
                    .col(AcknowledgedDerivation::Pname)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AcknowledgedDerivation::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AcknowledgedDerivation {
    Table,
    Id,
    Derivation,
    Pname,
    Note,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Derivation {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
