/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Split the monolithic `build` row into two concepts:
//!
//! * `derivation` — the immutable spec (path, architecture, outputs, deps,
//!   features). Keyed per-organization. Its `derivation_output` children
//!   carry content-addressed metadata (hashes, sizes, cache state) so that
//!   once populated, every future evaluation of the same drv reuses them.
//! * `build` — a per-evaluation attempt pointing at a derivation. Holds only
//!   the attempt-specific fields (status, server, log_id, build_time_ms).
//!
//! This migration drops the old `build_output` / `build_dependency` /
//! `build_feature` / `build_output_signature` tables and clears rows from
//! `build` (`derivation_path` and `architecture` columns are removed and
//! there is no way to backfill the new `derivation` FK). Pre-release
//! product, so losing historical build data is acceptable.
//!
//! It also introduces `cache_derivation`, a presence-tracking table so the
//! cacher can answer "does cache X have the full closure of derivation Y?"
//! with a single DB lookup instead of walking the filesystem.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ---- 1. Drop the old build_* satellite tables (signatures first so
        //         its FKs don't block the build_output drop).
        manager
            .drop_table(
                Table::drop()
                    .table(BuildOutputSignature::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(BuildOutput::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(BuildDependency::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(BuildFeature::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        // ---- 2. entry_point has a FK into build. Drop its build-referencing
        //         rows, then drop its FK so we can truncate build.
        let db = manager.get_connection();
        db.execute_unprepared("DELETE FROM entry_point").await?;
        db.execute_unprepared("DELETE FROM build").await?;

        // ---- 3. Remove the columns that now live on `derivation`.
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_column(Build::DerivationPath)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .drop_column(Build::Architecture)
                    .to_owned(),
            )
            .await?;

        // ---- 4. Create `derivation` (per-org, unique on drv path).
        manager
            .create_table(
                Table::create()
                    .table(Derivation::Table)
                    .col(
                        ColumnDef::new(Derivation::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Derivation::Organization).uuid().not_null())
                    .col(
                        ColumnDef::new(Derivation::DerivationPath)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Derivation::Architecture)
                            .small_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Derivation::CreatedAt).date_time().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation-organization")
                            .from(Derivation::Table, Derivation::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-derivation-org-path")
                    .table(Derivation::Table)
                    .col(Derivation::Organization)
                    .col(Derivation::DerivationPath)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ---- 5. Create `derivation_output` (one row per output name).
        manager
            .create_table(
                Table::create()
                    .table(DerivationOutput::Table)
                    .col(
                        ColumnDef::new(DerivationOutput::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DerivationOutput::Derivation)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DerivationOutput::Name).string().not_null())
                    .col(ColumnDef::new(DerivationOutput::Output).string().not_null())
                    .col(ColumnDef::new(DerivationOutput::Hash).string().not_null())
                    .col(
                        ColumnDef::new(DerivationOutput::Package)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DerivationOutput::Ca).string())
                    .col(ColumnDef::new(DerivationOutput::FileHash).string())
                    .col(ColumnDef::new(DerivationOutput::FileSize).big_integer())
                    .col(ColumnDef::new(DerivationOutput::NarSize).big_integer())
                    .col(
                        ColumnDef::new(DerivationOutput::IsCached)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DerivationOutput::HasArtefacts)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DerivationOutput::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_output-derivation")
                            .from(DerivationOutput::Table, DerivationOutput::Derivation)
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-derivation_output-derivation-name")
                    .table(DerivationOutput::Table)
                    .col(DerivationOutput::Derivation)
                    .col(DerivationOutput::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ---- 6. Create `derivation_dependency` (immutable graph edges).
        manager
            .create_table(
                Table::create()
                    .table(DerivationDependency::Table)
                    .col(
                        ColumnDef::new(DerivationDependency::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DerivationDependency::Derivation)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DerivationDependency::Dependency)
                            .uuid()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_dependency-derivation")
                            .from(
                                DerivationDependency::Table,
                                DerivationDependency::Derivation,
                            )
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_dependency-dependency")
                            .from(
                                DerivationDependency::Table,
                                DerivationDependency::Dependency,
                            )
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-derivation_dependency-pair")
                    .table(DerivationDependency::Table)
                    .col(DerivationDependency::Derivation)
                    .col(DerivationDependency::Dependency)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ---- 7. Create `derivation_feature` (features required to build).
        manager
            .create_table(
                Table::create()
                    .table(DerivationFeature::Table)
                    .col(
                        ColumnDef::new(DerivationFeature::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DerivationFeature::Derivation)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DerivationFeature::Feature).uuid().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_feature-derivation")
                            .from(DerivationFeature::Table, DerivationFeature::Derivation)
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_feature-feature")
                            .from(DerivationFeature::Table, DerivationFeature::Feature)
                            .to(Feature::Table, Feature::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-derivation_feature-pair")
                    .table(DerivationFeature::Table)
                    .col(DerivationFeature::Derivation)
                    .col(DerivationFeature::Feature)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ---- 8. Create `derivation_output_signature` (re-parented from
        //         `build_output_signature`).
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
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DerivationOutputSignature::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_output_signature-output")
                            .from(
                                DerivationOutputSignature::Table,
                                DerivationOutputSignature::DerivationOutput,
                            )
                            .to(DerivationOutput::Table, DerivationOutput::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_output_signature-cache")
                            .from(
                                DerivationOutputSignature::Table,
                                DerivationOutputSignature::Cache,
                            )
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ---- 9. Create `cache_derivation` (full-closure presence tracking).
        manager
            .create_table(
                Table::create()
                    .table(CacheDerivation::Table)
                    .col(
                        ColumnDef::new(CacheDerivation::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CacheDerivation::Cache).uuid().not_null())
                    .col(
                        ColumnDef::new(CacheDerivation::Derivation)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CacheDerivation::CachedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(ColumnDef::new(CacheDerivation::LastFetchedAt).date_time())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_derivation-cache")
                            .from(CacheDerivation::Table, CacheDerivation::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_derivation-derivation")
                            .from(CacheDerivation::Table, CacheDerivation::Derivation)
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-cache_derivation-pair")
                    .table(CacheDerivation::Table)
                    .col(CacheDerivation::Cache)
                    .col(CacheDerivation::Derivation)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ---- 10. Add build.derivation FK (now that derivation exists).
        manager
            .alter_table(
                Table::alter()
                    .table(Build::Table)
                    .add_column(ColumnDef::new(Build::Derivation).uuid().not_null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk-build-derivation")
                    .from(Build::Table, Build::Derivation)
                    .to(Derivation::Table, Derivation::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // One-way migration. Reverting would require re-synthesizing the old
        // denormalized schema, which is not useful at this stage of the
        // project — use `fresh` to rebuild from scratch.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Derivation,
    DerivationPath,
    Architecture,
}

#[derive(DeriveIden)]
enum BuildOutput {
    Table,
}

#[derive(DeriveIden)]
enum BuildDependency {
    Table,
}

#[derive(DeriveIden)]
enum BuildFeature {
    Table,
}

#[derive(DeriveIden)]
enum BuildOutputSignature {
    Table,
}

#[derive(DeriveIden)]
#[allow(clippy::enum_variant_names)]
enum Derivation {
    Table,
    Id,
    Organization,
    DerivationPath,
    Architecture,
    CreatedAt,
}

#[derive(DeriveIden)]
enum DerivationOutput {
    Table,
    Id,
    Derivation,
    Name,
    Output,
    Hash,
    Package,
    Ca,
    FileHash,
    FileSize,
    NarSize,
    IsCached,
    HasArtefacts,
    CreatedAt,
}

#[derive(DeriveIden)]
enum DerivationDependency {
    Table,
    Id,
    Derivation,
    Dependency,
}

#[derive(DeriveIden)]
enum DerivationFeature {
    Table,
    Id,
    Derivation,
    Feature,
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

#[derive(DeriveIden)]
enum CacheDerivation {
    Table,
    Id,
    Cache,
    Derivation,
    CachedAt,
    LastFetchedAt,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Feature {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}
