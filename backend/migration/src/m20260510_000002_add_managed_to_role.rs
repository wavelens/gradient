/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds `managed` to the `role` table so state-managed custom roles (defined
//! in `gradient-state.nix`) can be marked as such. Existing rows default to
//! `false`, preserving today's behavior - the three built-in roles
//! (Admin/Write/View) and any user-created custom roles remain unmanaged.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Role::Table)
                    .add_column(
                        ColumnDef::new(Role::Managed)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Role::Table)
                    .drop_column(Role::Managed)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Role {
    Table,
    Managed,
}
