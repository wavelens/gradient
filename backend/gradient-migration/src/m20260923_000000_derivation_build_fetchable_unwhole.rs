/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A row that is fetchable with a runtime hole counted contradicts the readiness
//! predicate, and the sweep's readiness repair now names every such row in its
//! scope. The set is empty on a healthy fleet, so the index that answers the scan
//! costs nothing to keep and turns a table read per sweep into none.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-fetchable-unwhole\" \
     ON derivation_build (derivation) WHERE fetchable AND missing_runtime_deps > 0",
];

const DOWN: &[&str] = &["DROP INDEX IF EXISTS \"idx-derivation_build-fetchable-unwhole\""];

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

    /// The predicate is spelled as `readiness::repair_scope` spells its arm, so the
    /// planner can prove the one implies the other.
    #[test]
    fn the_predicate_is_the_scopes_contradiction_arm() {
        assert!(
            UP[0].contains("WHERE fetchable AND missing_runtime_deps > 0"),
            "{}",
            UP[0]
        );
        assert!(UP[0].contains("derivation_build (derivation)"), "{}", UP[0]);
    }
}
