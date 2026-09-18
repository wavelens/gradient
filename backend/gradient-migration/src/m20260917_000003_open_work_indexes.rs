/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Finding the open work without reading the finished work. A settled server
//! holds hundreds of thousands of closed anchors and attempts and a handful of
//! open ones, so every statement that looked for open work read the whole table
//! to find it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-active\" \
     ON derivation_build (derivation) WHERE status IN (0, 1, 2, 8)",
    "CREATE INDEX IF NOT EXISTS \"idx-build_attempt-deterministic-failure\" \
     ON build_attempt (derivation_build) WHERE outcome = 3 AND reason = 5",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_build-active\"",
    "DROP INDEX IF EXISTS \"idx-build_attempt-deterministic-failure\"",
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

    /// Postgres only uses a partial index where it can prove the query's
    /// predicate implies the index's, so each predicate has to name what the
    /// statements name: the builder statuses Created, Queued, Building and
    /// Substituting, and `promotion`'s deterministic build failure.
    #[test]
    fn each_predicate_names_what_the_statements_ask_for() {
        let anchors = UP[0];
        let attempts = UP[1];

        assert!(
            anchors.contains("WHERE status IN (0, 1, 2, 8)"),
            "{anchors}"
        );
        assert!(
            anchors.contains("derivation_build (derivation)"),
            "{anchors}"
        );
        assert!(
            attempts.contains("WHERE outcome = 3 AND reason = 5"),
            "{attempts}"
        );
        assert!(
            attempts.contains("build_attempt (derivation_build)"),
            "{attempts}"
        );
    }
}
