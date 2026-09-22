/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Finding the open anchors without reading the settled ones. Every walk reaches
//! an anchor while it is not fetchable and not the requeue's, and the sweep's
//! demand recount and both naming probes scan for exactly that, where a status
//! list used to leave an unwhole `Completed` anchor out of every index.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &["CREATE INDEX IF NOT EXISTS \"idx-derivation_build-open\" \
     ON derivation_build (derivation) WHERE NOT fetchable AND status NOT IN (4, 5, 6, 9)"];

const DOWN: &[&str] = &["DROP INDEX IF EXISTS \"idx-derivation_build-open\""];

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

    /// Postgres only uses a partial index where it can prove the query's predicate
    /// implies the index's, so this has to spell `graph_sql::open_predicate` as the
    /// statements do: not fetchable, and not `BuildStatus::REQUEUEABLE`.
    #[test]
    fn the_predicate_is_open_as_the_statements_spell_it() {
        assert!(
            UP[0].contains("WHERE NOT fetchable AND status NOT IN (4, 5, 6, 9)"),
            "{}",
            UP[0]
        );
        assert!(UP[0].contains("derivation_build (derivation)"), "{}", UP[0]);
    }
}
