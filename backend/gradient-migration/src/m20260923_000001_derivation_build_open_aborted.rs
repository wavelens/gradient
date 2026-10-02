/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The open index predicate must follow `graph_sql::open_predicate`. An aborted shared build is
//! open because an abort is not a verdict.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_build-open\"",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-open\" \
     ON derivation_build (derivation) WHERE NOT fetchable AND status NOT IN (4, 6, 9)",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_build-open\"",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-open\" \
     ON derivation_build (derivation) WHERE NOT fetchable AND status NOT IN (4, 5, 6, 9)",
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
