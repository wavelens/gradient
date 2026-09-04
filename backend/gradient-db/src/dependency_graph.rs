/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared graph walks over the `derivation_dependency` table.
//!
//! The `derivation_dependency` row `(derivation, dependency)` means
//! "`derivation` depends on `dependency`". A *reverse* walk from a starting
//! derivation therefore yields its transitive **dependents** - every derivation
//! that (directly or indirectly) needs the start node to be available.
//!
//! Two callers historically reimplemented the same BFS with subtly different
//! shapes (cache invalidation closure revocation, build-failure cascade); this
//! module hosts the single canonical version.

use crate::graph_sql::{ClosureDirection, dependency_closure_cte};
use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, DatabaseBackend, FromQueryResult, Statement};
use std::collections::HashSet;

use gradient_types::*;

#[derive(FromQueryResult)]
struct DerivationRow {
    derivation: uuid::Uuid,
}

/// Returns the set of all transitive dependents of `start`, **including** `start`
/// itself, as one recursive statement over the reverse `derivation_dependency`
/// edges.
///
/// A start node nothing depends on ⇒ result contains exactly `{start}`.
pub async fn collect_transitive_dependents<C: ConnectionTrait>(
    db: &C,
    start: DerivationId,
) -> Result<HashSet<DerivationId>> {
    let sql = format!(
        "{} SELECT derivation FROM dependents",
        dependency_closure_cte(
            "dependents",
            "SELECT $1::uuid",
            ClosureDirection::Dependents,
        )
    );
    let rows = DerivationRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        [start.into_inner().into()],
    ))
    .all(db)
    .await
    .context("walk derivation_dependency reverse edges")?;

    Ok(rows
        .into_iter()
        .map(|r| DerivationId::new(r.derivation))
        .chain(std::iter::once(start))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn node(derivation: DerivationId) -> MDerivationDependency {
        gradient_entity::derivation_dependency::Model {
            derivation,
            dependency: derivation,
        }
    }

    /// A derivation nothing depends on still reports itself, so callers can
    /// treat the result as "everything this change touches" without special
    /// casing the start node.
    #[tokio::test]
    async fn no_dependents_returns_only_start() {
        let start = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MDerivationDependency>::new()])
            .into_connection();

        let visited = collect_transitive_dependents(&db, start).await.unwrap();

        assert_eq!(visited.len(), 1);
        assert!(visited.contains(&start));
    }

    /// The walk seeds itself, so `start` comes back from the database as well as
    /// from the chain; the set must hold one copy.
    #[tokio::test]
    async fn start_is_not_double_counted() {
        let a = DerivationId::now_v7();
        let b = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![node(a), node(b)]])
            .into_connection();

        let visited = collect_transitive_dependents(&db, a).await.unwrap();

        assert_eq!(visited.len(), 2);
        assert!(visited.contains(&a) && visited.contains(&b));
    }
}
