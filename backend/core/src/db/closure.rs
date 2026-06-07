/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Transitive build-closure walks and output-size summation.
//!
//! A `derivation_dependency` row `(derivation, dependency)` means
//! "`derivation` depends on `dependency`". A *forward* walk from a set of root
//! derivations therefore yields the full set of derivations that must be built
//! or substituted to realise the roots. The coalesced output NAR size summed
//! over that set is the closure size used by the build-closure endpoint and by
//! the scheduler's scoring context.

use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet};

use crate::types::*;

/// BFS over forward `derivation_dependency` edges from `roots`; returns every
/// reachable derivation id (roots included).
pub async fn transitive_closure_reachable<C: ConnectionTrait>(
    db: &C,
    roots: &[DerivationId],
) -> Result<HashSet<DerivationId>, DbErr> {
    let mut visited: HashSet<DerivationId> = roots.iter().copied().collect();
    let mut frontier: Vec<DerivationId> = roots.to_vec();

    while !frontier.is_empty() {
        let edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.is_in(frontier.clone()))
            .all(db)
            .await?;
        frontier.clear();
        for edge in edges {
            if visited.insert(edge.dependency) {
                frontier.push(edge.dependency);
            }
        }
    }

    Ok(visited)
}

/// Map each derivation id to its coalesced output NAR size
/// (`derivation_output.nar_size`, else matching `cached_path.nar_size`).
pub async fn output_sizes_by_drv<C: ConnectionTrait>(
    db: &C,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, i64>, DbErr> {
    if drv_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(drv_ids.to_vec()))
        .all(db)
        .await?;

    let missing_hashes: Vec<String> = outputs
        .iter()
        .filter(|o| o.nar_size.is_none())
        .map(|o| o.hash.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let cached_size_by_hash: HashMap<String, i64> = if missing_hashes.is_empty() {
        HashMap::new()
    } else {
        ECachedPath::find()
            .filter(CCachedPath::Hash.is_in(missing_hashes))
            .all(db)
            .await?
            .into_iter()
            .filter_map(|cp| cp.nar_size.map(|n| (cp.hash, n)))
            .collect()
    };

    let mut by_drv: HashMap<DerivationId, i64> = HashMap::new();
    for o in outputs {
        if let Some(size) = o
            .nar_size
            .or_else(|| cached_size_by_hash.get(&o.hash).copied())
        {
            *by_drv.entry(o.derivation).or_insert(0) += size;
        }
    }
    Ok(by_drv)
}

/// Total coalesced output NAR size of the full build closure seeded at `roots`.
/// Returns `0` for an empty closure or one with no known sizes.
pub async fn transitive_closure_size<C: ConnectionTrait>(
    db: &C,
    roots: &[DerivationId],
) -> Result<i64, DbErr> {
    let closure = transitive_closure_reachable(db, roots).await?;
    let all_ids: Vec<DerivationId> = closure.into_iter().collect();
    let by_drv = output_sizes_by_drv(db, &all_ids).await?;
    Ok(by_drv.values().sum())
}

/// Closure size for many roots at once. Loads the dependency graph and output
/// sizes for the combined reachable set in a handful of batched queries, then
/// sums each root's closure in memory (diamonds deduped via a per-root visited
/// set). This is `O(depth)` DB round-trips for the whole batch instead of one
/// full DB walk per root, which matters when a dispatch round backfills many
/// derivations that share most of their closure.
pub async fn transitive_closure_sizes<C: ConnectionTrait>(
    db: &C,
    roots: &[DerivationId],
) -> Result<HashMap<DerivationId, i64>, DbErr> {
    if roots.is_empty() {
        return Ok(HashMap::new());
    }

    let mut adjacency: HashMap<DerivationId, Vec<DerivationId>> = HashMap::new();
    let mut reachable: HashSet<DerivationId> = roots.iter().copied().collect();
    let mut frontier: Vec<DerivationId> = roots.to_vec();
    while !frontier.is_empty() {
        let edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.is_in(frontier.clone()))
            .all(db)
            .await?;
        frontier.clear();
        for edge in edges {
            adjacency.entry(edge.derivation).or_default().push(edge.dependency);
            if reachable.insert(edge.dependency) {
                frontier.push(edge.dependency);
            }
        }
    }

    let sizes = output_sizes_by_drv(db, &reachable.iter().copied().collect::<Vec<_>>()).await?;

    let mut result: HashMap<DerivationId, i64> = HashMap::with_capacity(roots.len());
    for &root in roots {
        let mut visited: HashSet<DerivationId> = HashSet::from([root]);
        let mut stack = vec![root];
        let mut total = 0i64;
        while let Some(node) = stack.pop() {
            total += sizes.get(&node).copied().unwrap_or(0);
            if let Some(children) = adjacency.get(&node) {
                for &child in children {
                    if visited.insert(child) {
                        stack.push(child);
                    }
                }
            }
        }
        result.insert(root, total);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::{derivation_dependency, derivation_output};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn now() -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc()
    }

    fn out(derivation: DerivationId, hash: &str, nar_size: Option<i64>) -> derivation_output::Model {
        derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation,
            name: "out".into(),
            hash: hash.into(),
            package: "foo".into(),
            nar_size,
            created_at: now(),
            ..Default::default()
        }
    }

    fn dep(derivation: DerivationId, dependency: DerivationId) -> derivation_dependency::Model {
        derivation_dependency::Model {
            id: DerivationDependencyId::now_v7(),
            derivation,
            dependency,
        }
    }

    #[tokio::test]
    async fn sums_closure_output_sizes() {
        let root = DerivationId::now_v7();
        let child = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // reachable: wave 1 (root) -> edge root->child
            .append_query_results([vec![dep(root, child)]])
            // wave 2 (child) -> no further edges
            .append_query_results([Vec::<derivation_dependency::Model>::new()])
            // output_sizes_by_drv: outputs for [root, child]
            .append_query_results([vec![out(root, "r", Some(100)), out(child, "c", Some(40))]])
            .into_connection();

        let total = transitive_closure_size(&db, &[root]).await.unwrap();
        assert_eq!(total, 140);
    }

    #[tokio::test]
    async fn empty_roots_is_zero() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let total = transitive_closure_size(&db, &[]).await.unwrap();
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn bulk_sizes_dedup_diamond() {
        // root -> a, root -> b, a -> c, b -> c. c must be counted once.
        let root = DerivationId::now_v7();
        let a = DerivationId::now_v7();
        let b = DerivationId::now_v7();
        let c = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![dep(root, a), dep(root, b)]])
            .append_query_results([vec![dep(a, c), dep(b, c)]])
            .append_query_results([Vec::<derivation_dependency::Model>::new()])
            .append_query_results([vec![
                out(root, "r", Some(10)),
                out(a, "a", Some(20)),
                out(b, "b", Some(30)),
                out(c, "c", Some(40)),
            ]])
            .into_connection();

        let sizes = transitive_closure_sizes(&db, &[root]).await.unwrap();
        assert_eq!(sizes.get(&root).copied(), Some(100));
    }
}
