/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fleet-shared Nix eval-cache: `eval_cache_store` is a blob registry keyed by
//! flake fingerprint, and `evaluation.cache_status` records whether an eval
//! pulled a warm cache (#386).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(EvalCacheStore::Table)
                    .col(
                        ColumnDef::new(EvalCacheStore::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EvalCacheStore::Fingerprint)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(EvalCacheStore::StoragePath).text().not_null())
                    .col(
                        ColumnDef::new(EvalCacheStore::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EvalCacheStore::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EvalCacheStore::UpdatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-eval_cache_store-fingerprint")
                    .table(EvalCacheStore::Table)
                    .col(EvalCacheStore::Fingerprint)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .add_column(
                        ColumnDef::new(Evaluation::CacheStatus)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Evaluation::Table)
                    .drop_column(Evaluation::CacheStatus)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(EvalCacheStore::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum EvalCacheStore {
    Table,
    Id,
    Fingerprint,
    StoragePath,
    SizeBytes,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    CacheStatus,
}
