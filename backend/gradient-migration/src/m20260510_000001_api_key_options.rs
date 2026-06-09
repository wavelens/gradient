/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds configurable options to API keys: a permission bitmask (subset of
//! `gradient_core::permissions::Permission::ALL`) and an optional organization
//! pin. Existing rows receive `permission = admin_mask()` and `organization =
//! NULL` to preserve today's "key has all the powers of its user, in any org"
//! behavior. The org pin uses `ON DELETE SET NULL` so that deleting an
//! organization unpins affected keys rather than destroying them.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Equal to `gradient_core::permissions::admin_mask()` at the time this
/// migration is authored. Hard-coding the literal keeps the migration
/// independent of crate-level changes to `Permission::ALL` ordering.
const ADMIN_MASK: i64 = 0x1FFF; // bits 0..=12 set

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Api::Table)
                    .add_column(
                        ColumnDef::new(Api::Permission)
                            .big_integer()
                            .not_null()
                            .default(ADMIN_MASK),
                    )
                    .add_column(ColumnDef::new(Api::Organization).uuid().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_api_organization")
                    .from(Api::Table, Api::Organization)
                    .to(Organization::Table, Organization::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .on_update(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_api_organization")
                    .table(Api::Table)
                    .col(Api::Organization)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_api_organization")
                    .table(Api::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_api_organization")
                    .table(Api::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Api::Table)
                    .drop_column(Api::Permission)
                    .drop_column(Api::Organization)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Api {
    Table,
    Permission,
    Organization,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
}
