/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Indexes the hash/derivation columns that the cache write path filters on but
//! that were previously only sequential-scanned:
//!
//! 1. `idx-derivation_output-hash` - NAR ingest (`proto::handler::nar`) and the
//!    sign sweep both look up `derivation_output` by `hash`; without this index
//!    every NAR push and every sweep degenerated into a full table scan.
//! 2. `idx-build-derivation` - the sign-sweep join `build.derivation =
//!    derivation.id` could not use the composite `(evaluation, derivation)`
//!    index (derivation is the trailing column), so it scanned all of `build`.
//! 3. `idx-cached_path-hash` - cache invalidation and cleanup look up
//!    `cached_path` by `hash`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .name("idx-derivation_output-hash")
                    .table(DerivationOutput::Table)
                    .col(DerivationOutput::Hash)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-build-derivation")
                    .table(Build::Table)
                    .col(Build::Derivation)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-cached_path-hash")
                    .table(CachedPath::Table)
                    .col(CachedPath::Hash)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx-cached_path-hash")
                    .table(CachedPath::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx-build-derivation")
                    .table(Build::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx-derivation_output-hash")
                    .table(DerivationOutput::Table)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum DerivationOutput {
    Table,
    Hash,
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Derivation,
}

#[derive(DeriveIden)]
enum CachedPath {
    Table,
    Hash,
}
