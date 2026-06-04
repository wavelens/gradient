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
            ca: None,
            nar_size,
            is_cached: false,
            cached_path: None,
            created_at: now(),
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
}
