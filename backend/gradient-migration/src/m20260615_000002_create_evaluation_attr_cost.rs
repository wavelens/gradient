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
                    .table(EvaluationAttrCost::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EvaluationAttrCost::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(EvaluationAttrCost::Evaluation).uuid().not_null())
                    .col(ColumnDef::new(EvaluationAttrCost::Attr).string().not_null())
                    .col(ColumnDef::new(EvaluationAttrCost::Thunks).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationAttrCost::FnCalls).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationAttrCost::EvalMs).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationAttrCost::AllocBytes).big_integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-evaluation_attr_cost-evaluation")
                            .from(EvaluationAttrCost::Table, EvaluationAttrCost::Evaluation)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-evaluation_attr_cost-evaluation")
                    .table(EvaluationAttrCost::Table)
                    .col(EvaluationAttrCost::Evaluation)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-evaluation_attr_cost-attr")
                    .table(EvaluationAttrCost::Table)
                    .col(EvaluationAttrCost::Attr)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EvaluationAttrCost::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum EvaluationAttrCost {
    Table,
    Id,
    Evaluation,
    Attr,
    Thunks,
    FnCalls,
    EvalMs,
    AllocBytes,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
}
