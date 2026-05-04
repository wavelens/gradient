/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared graph walks over the `derivation_dependency` table.
//!
//! The `derivation_dependency` row `(derivation, dependency)` means
//! "`derivation` depends on `dependency`". A *reverse* walk from a starting
//! derivation therefore yields its transitive **dependents** — every derivation
//! that (directly or indirectly) needs the start node to be available.
//!
//! Two callers historically reimplemented the same BFS with subtly different
//! shapes (cache invalidation closure revocation, build-failure cascade); this
//! module hosts the single canonical version.

use anyhow::{Context, Result};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::collections::HashSet;
use uuid::Uuid;

use crate::types::*;

/// Returns the set of all transitive dependents of `start`, **including** `start`
/// itself. Walks reverse `derivation_dependency` edges in BFS layers, batching
/// each layer into a single `IS IN` query.
///
/// Empty input (caller passes `start` only) ⇒ result contains exactly `{start}`.
pub async fn collect_transitive_dependents<C: ConnectionTrait>(
    db: &C,
    start: Uuid,
) -> Result<HashSet<Uuid>> {
    let mut visited: HashSet<Uuid> = HashSet::new();
    visited.insert(start);
    let mut frontier: Vec<Uuid> = vec![start];

    while !frontier.is_empty() {
        let edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Dependency.is_in(frontier.clone()))
            .all(db)
            .await
            .context("walk derivation_dependency reverse edges")?;
        frontier.clear();
        for edge in edges {
            if visited.insert(edge.derivation) {
                frontier.push(edge.derivation);
            }
        }
    }

    Ok(visited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn dep_edge(derivation: Uuid, dependency: Uuid) -> MDerivationDependency {
        entity::derivation_dependency::Model {
            id: Uuid::now_v7(),
            derivation,
            dependency,
        }
    }

    #[tokio::test]
    async fn no_dependents_returns_only_start() {
        let start = Uuid::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MDerivationDependency>::new()])
            .into_connection();

        let visited = collect_transitive_dependents(&db, start).await.unwrap();
        assert_eq!(visited.len(), 1);
        assert!(visited.contains(&start));
    }

    #[tokio::test]
    async fn walks_multiple_layers_breadth_first() {
        let a = Uuid::now_v7(); // start
        let b = Uuid::now_v7(); // depends on a
        let c = Uuid::now_v7(); // depends on b
        let d = Uuid::now_v7(); // depends on a (sibling of b)

        // Layer 1: dependents of {a}        → b, d
        // Layer 2: dependents of {b, d}     → c
        // Layer 3: dependents of {c}        → ∅
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![dep_edge(b, a), dep_edge(d, a)]])
            .append_query_results([vec![dep_edge(c, b)]])
            .append_query_results([Vec::<MDerivationDependency>::new()])
            .into_connection();

        let visited = collect_transitive_dependents(&db, a).await.unwrap();
        assert_eq!(visited.len(), 4);
        for id in [a, b, c, d] {
            assert!(visited.contains(&id), "missing {}", id);
        }
    }

    #[tokio::test]
    async fn cycles_terminate() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        // Pathological cycle: b depends on a AND a depends on b. The visited
        // set must dedupe so the BFS terminates.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![dep_edge(b, a)]])
            .append_query_results([vec![dep_edge(a, b)]])
            .into_connection();

        let visited = collect_transitive_dependents(&db, a).await.unwrap();
        assert_eq!(visited.len(), 2);
    }
}
