/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! An aborted anchor is open: an abort is not a verdict, and a live want thaws
//! it the way it thaws a skipped one. The open index's predicate follows
//! `graph_sql::open_predicate`, which now stops only at the terminal failures.

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

#[cfg(test)]
mod tests {
    use super::UP;

    /// The predicate spells `graph_sql::open_predicate`: not fetchable, and not
    /// `BuildStatus::TERMINAL_FAILURE`, so the planner can prove the statements'
    /// predicate implies it.
    #[test]
    fn the_predicate_is_open_with_aborted_inside() {
        assert!(
            UP[1].contains("WHERE NOT fetchable AND status NOT IN (4, 6, 9)"),
            "{}",
            UP[1]
        );
        assert!(UP[1].contains("derivation_build (derivation)"), "{}", UP[1]);
    }
}
