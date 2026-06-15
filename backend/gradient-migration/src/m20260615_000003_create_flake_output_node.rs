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
                    .table(FlakeOutputNode::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FlakeOutputNode::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FlakeOutputNode::Evaluation).uuid().not_null())
                    .col(ColumnDef::new(FlakeOutputNode::Path).string().not_null())
                    .col(ColumnDef::new(FlakeOutputNode::Parent).string().null())
                    .col(ColumnDef::new(FlakeOutputNode::Name).string().not_null())
                    .col(ColumnDef::new(FlakeOutputNode::Kind).string().not_null())
                    .col(ColumnDef::new(FlakeOutputNode::IsDerivation).boolean().not_null())
                    .col(ColumnDef::new(FlakeOutputNode::DrvPath).string().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-flake_output_node-evaluation")
                            .from(FlakeOutputNode::Table, FlakeOutputNode::Evaluation)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-flake_output_node-evaluation-parent")
                    .table(FlakeOutputNode::Table)
                    .col(FlakeOutputNode::Evaluation)
                    .col(FlakeOutputNode::Parent)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FlakeOutputNode::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum FlakeOutputNode {
    Table,
    Id,
    Evaluation,
    Path,
    Parent,
    Name,
    Kind,
    IsDerivation,
    DrvPath,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
}
