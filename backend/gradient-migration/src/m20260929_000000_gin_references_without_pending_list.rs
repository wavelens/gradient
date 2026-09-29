/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The reference indexes serve point lookups from the walk, and a GIN index with
//! `fastupdate` reads its whole pending list, up to `gin_pending_list_limit`, on
//! every probe. Inserts now go straight into the tree, and the list history left
//! behind is merged once here.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"ALTER INDEX "idx-cached_path-references" SET (fastupdate = off)"#,
    r#"ALTER INDEX "idx-derivation_output-references_list" SET (fastupdate = off)"#,
    r#"SELECT gin_clean_pending_list('"idx-cached_path-references"'),
            gin_clean_pending_list('"idx-derivation_output-references_list"')"#,
];

const DOWN: &[&str] = &[
    r#"ALTER INDEX "idx-cached_path-references" RESET (fastupdate)"#,
    r#"ALTER INDEX "idx-derivation_output-references_list" RESET (fastupdate)"#,
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in UP {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in DOWN {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
