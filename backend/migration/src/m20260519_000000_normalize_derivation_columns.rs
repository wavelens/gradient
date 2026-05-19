/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Split `derivation.derivation_path` into structured `hash` + `name`
//! columns and drop the redundant `derivation_output.output` (the full
//! `/nix/store/<hash>-<package>` path, reconstructed on read from
//! `hash` + `package`).
//!
//! The old schema stored each `<hash>-<name>.drv` derivation path as a
//! single string and re-parsed it on every read to display a build's name.
//! When an evaluation contained tens of thousands of builds, the
//! `GET /evals/{eval}/builds` endpoint hydrated derivations via
//! `WHERE id IN ($1, $2, ..., $N)`, which overflowed Postgres' 65 535
//! parameter limit (issue #237). Structured columns let the endpoint sort
//! by `derivation.name` directly and read the display name without
//! per-row parsing.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .add_column(ColumnDef::new(Derivation::Hash).text().null())
                    .add_column(ColumnDef::new(Derivation::Name).text().null())
                    .to_owned(),
            )
            .await?;

        let db = manager.get_connection();
        let backend = db.get_database_backend();

        db.execute(Statement::from_string(
            backend,
            "UPDATE derivation \
             SET hash = split_part(derivation_path, '-', 1), \
                 name = regexp_replace( \
                     substring(derivation_path FROM position('-' IN derivation_path) + 1), \
                     '\\.drv$', '' \
                 ) \
             WHERE derivation_path LIKE '%-%'",
        ))
        .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .modify_column(ColumnDef::new(Derivation::Hash).text().not_null())
                    .modify_column(ColumnDef::new(Derivation::Name).text().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx-derivation-org-path")
                    .table(Derivation::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-derivation-org-hash-name")
                    .table(Derivation::Table)
                    .col(Derivation::Organization)
                    .col(Derivation::Hash)
                    .col(Derivation::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .drop_column(Derivation::DerivationPath)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DerivationOutput::Table)
                    .drop_column(DerivationOutput::Output)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        manager
            .alter_table(
                Table::alter()
                    .table(DerivationOutput::Table)
                    .add_column(ColumnDef::new(DerivationOutput::Output).text().null())
                    .to_owned(),
            )
            .await?;

        db.execute(Statement::from_string(
            backend,
            "UPDATE derivation_output \
             SET output = '/nix/store/' || hash || '-' || package",
        ))
        .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DerivationOutput::Table)
                    .modify_column(ColumnDef::new(DerivationOutput::Output).text().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .add_column(ColumnDef::new(Derivation::DerivationPath).text().null())
                    .to_owned(),
            )
            .await?;

        db.execute(Statement::from_string(
            backend,
            "UPDATE derivation SET derivation_path = hash || '-' || name || '.drv'",
        ))
        .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .modify_column(
                        ColumnDef::new(Derivation::DerivationPath)
                            .text()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx-derivation-org-hash-name")
                    .table(Derivation::Table)
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

        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .drop_column(Derivation::Hash)
                    .drop_column(Derivation::Name)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
#[allow(clippy::enum_variant_names)]
enum Derivation {
    Table,
    Organization,
    DerivationPath,
    Hash,
    Name,
}

#[derive(DeriveIden)]
enum DerivationOutput {
    Table,
    Output,
}
