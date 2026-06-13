/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Caches powering fast entry-point dependency-closure counts (#383):
//! `derivation_closure` materialises a derivation's build-time closure once
//! (content-addressed, reused across evals), `derivation.dep_closure_count`
//! caches its size, and `entry_point_dep_count` holds the per-entry-point
//! build-status histogram maintained incrementally as builds transition.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DerivationClosure::Table)
                    .col(
                        ColumnDef::new(DerivationClosure::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DerivationClosure::RootDerivation)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DerivationClosure::DepDerivation)
                            .uuid()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_closure-root")
                            .from(DerivationClosure::Table, DerivationClosure::RootDerivation)
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-derivation_closure-dep")
                            .from(DerivationClosure::Table, DerivationClosure::DepDerivation)
                            .to(Derivation::Table, Derivation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-derivation_closure-pair")
                    .table(DerivationClosure::Table)
                    .col(DerivationClosure::RootDerivation)
                    .col(DerivationClosure::DepDerivation)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-derivation_closure-dep")
                    .table(DerivationClosure::Table)
                    .col(DerivationClosure::DepDerivation)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .add_column(ColumnDef::new(Derivation::DepClosureCount).big_integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(EntryPointDepCount::Table)
                    .col(
                        ColumnDef::new(EntryPointDepCount::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EntryPointDepCount::EntryPoint)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(EntryPointDepCount::Status).integer().not_null())
                    .col(
                        ColumnDef::new(EntryPointDepCount::Count)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-entry_point_dep_count-entry_point")
                            .from(EntryPointDepCount::Table, EntryPointDepCount::EntryPoint)
                            .to(EntryPoint::Table, EntryPoint::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-entry_point_dep_count-pair")
                    .table(EntryPointDepCount::Table)
                    .col(EntryPointDepCount::EntryPoint)
                    .col(EntryPointDepCount::Status)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EntryPointDepCount::Table).to_owned())
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Derivation::Table)
                    .drop_column(Derivation::DepClosureCount)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(DerivationClosure::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DerivationClosure {
    Table,
    Id,
    RootDerivation,
    DepDerivation,
}

#[derive(DeriveIden)]
enum EntryPointDepCount {
    Table,
    Id,
    EntryPoint,
    Status,
    Count,
}

#[derive(DeriveIden)]
enum Derivation {
    Table,
    Id,
    DepClosureCount,
}

#[derive(DeriveIden)]
enum EntryPoint {
    Table,
    Id,
}
