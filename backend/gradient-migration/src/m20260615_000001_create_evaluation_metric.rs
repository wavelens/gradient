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
                    .table(EvaluationMetric::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EvaluationMetric::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(EvaluationMetric::Evaluation).uuid().not_null())
                    .col(ColumnDef::new(EvaluationMetric::TotalThunks).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::FnCalls).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::PrimopCalls).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::Lookups).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::AllocBytes).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::PeakHeapMb).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::PeakRssMb).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::FetchMs).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::EvalFlakeMs).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::EvalDrvMs).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::TotalEvalMs).big_integer().not_null())
                    .col(ColumnDef::new(EvaluationMetric::WorkerId).string().not_null())
                    .col(ColumnDef::new(EvaluationMetric::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-evaluation_metric-evaluation")
                            .from(EvaluationMetric::Table, EvaluationMetric::Evaluation)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-evaluation_metric-evaluation")
                    .table(EvaluationMetric::Table)
                    .col(EvaluationMetric::Evaluation)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EvaluationMetric::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum EvaluationMetric {
    Table,
    Id,
    Evaluation,
    TotalThunks,
    FnCalls,
    PrimopCalls,
    Lookups,
    AllocBytes,
    PeakHeapMb,
    PeakRssMb,
    FetchMs,
    EvalFlakeMs,
    EvalDrvMs,
    TotalEvalMs,
    WorkerId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
}
