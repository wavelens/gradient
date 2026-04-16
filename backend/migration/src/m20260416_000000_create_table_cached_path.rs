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
        // ── 1. Create cached_path table ──────────────────────────────────────
        // One row per unique store path. NAR data is stored once by hash.
        manager
            .create_table(
                Table::create()
                    .table(CachedPath::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CachedPath::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CachedPath::StorePath).text().not_null())
                    .col(
                        ColumnDef::new(CachedPath::Hash)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(CachedPath::Package).text().not_null())
                    .col(ColumnDef::new(CachedPath::FileHash).text())
                    .col(ColumnDef::new(CachedPath::FileSize).big_integer())
                    .col(ColumnDef::new(CachedPath::NarSize).big_integer())
                    .col(ColumnDef::new(CachedPath::NarHash).text())
                    .col(ColumnDef::new(CachedPath::References).text())
                    .col(ColumnDef::new(CachedPath::Ca).text())
                    .col(
                        ColumnDef::new(CachedPath::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ── 2. Create cached_path_signature (many-to-many: path ↔ cache) ────
        // Each row associates a cached_path with a cache and optionally holds
        // a signature. Signature starts NULL, filled by signing jobs.
        manager
            .create_table(
                Table::create()
                    .table(CachedPathSignature::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CachedPathSignature::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CachedPathSignature::CachedPath)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CachedPathSignature::Cache)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(CachedPathSignature::Signature).text())
                    .col(
                        ColumnDef::new(CachedPathSignature::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cached_path_signature-cached_path")
                            .from(CachedPathSignature::Table, CachedPathSignature::CachedPath)
                            .to(CachedPath::Table, CachedPath::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cached_path_signature-cache")
                            .from(CachedPathSignature::Table, CachedPathSignature::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-cached_path_signature-cached_path-cache")
                    .table(CachedPathSignature::Table)
                    .col(CachedPathSignature::CachedPath)
                    .col(CachedPathSignature::Cache)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ── 3. Add nullable cached_path FK to derivation_output ──────────────
        manager
            .alter_table(
                Table::alter()
                    .table(DerivationOutput::Table)
                    .add_column(ColumnDef::new(DerivationOutput::CachedPath).uuid())
                    .add_foreign_key(
                        &TableForeignKey::new()
                            .name("fk-derivation_output-cached_path")
                            .from_tbl(DerivationOutput::Table)
                            .from_col(DerivationOutput::CachedPath)
                            .to_tbl(CachedPath::Table)
                            .to_col(CachedPath::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;

        // ── 4. Drop derivation_output_signature table ────────────────────────
        manager
            .drop_table(
                Table::drop()
                    .table(DerivationOutputSignature::Table)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Recreate derivation_output_signature (simplified — loses data)
        manager
            .create_table(
                Table::create()
                    .table(DerivationOutputSignature::Table)
                    .col(
                        ColumnDef::new(DerivationOutputSignature::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DerivationOutputSignature::DerivationOutput)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DerivationOutputSignature::Cache)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DerivationOutputSignature::Signature)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DerivationOutputSignature::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DerivationOutput::Table)
                    .drop_column(DerivationOutput::CachedPath)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(CachedPathSignature::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(CachedPath::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CachedPath {
    Table,
    Id,
    StorePath,
    Hash,
    Package,
    FileHash,
    FileSize,
    NarSize,
    NarHash,
    References,
    Ca,
    CreatedAt,
}

#[derive(DeriveIden)]
enum CachedPathSignature {
    Table,
    Id,
    CachedPath,
    Cache,
    Signature,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum DerivationOutput {
    Table,
    CachedPath,
}

#[derive(DeriveIden)]
enum DerivationOutputSignature {
    Table,
    Id,
    DerivationOutput,
    Cache,
    Signature,
    CreatedAt,
}
