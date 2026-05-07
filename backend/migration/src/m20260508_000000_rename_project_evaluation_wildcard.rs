/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Rename `project.evaluation_wildcard` to `project.wildcard` so the column
//! shares the same name as `evaluation.wildcard`. The two columns hold the
//! same kind of value (a `Wildcard` pattern) and the divergent naming was a
//! source of confusion. See issue #73.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project"))
                    .rename_column(Alias::new("evaluation_wildcard"), Alias::new("wildcard"))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project"))
                    .rename_column(Alias::new("wildcard"), Alias::new("evaluation_wildcard"))
                    .to_owned(),
            )
            .await
    }
}
