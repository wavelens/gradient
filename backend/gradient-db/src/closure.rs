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

use crate::graph_sql::{ClosureDirection, dependency_closure_cte};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, Statement,
};
use std::collections::{HashMap, HashSet};

use gradient_types::*;

#[derive(FromQueryResult)]
struct DerivationRow {
    derivation: uuid::Uuid,
}

#[derive(FromQueryResult)]
struct EdgeRow {
    derivation: uuid::Uuid,
    dependency: uuid::Uuid,
}

/// The `WITH RECURSIVE closure(derivation)` prelude seeded from a bound
/// `uuid[]` of roots. One statement replaces a level-at-a-time BFS that cost a
/// round trip per level (18 to 21 on production graphs).
fn roots_closure_cte() -> String {
    dependency_closure_cte(
        "closure",
        "SELECT unnest($1::uuid[])",
        ClosureDirection::Dependencies,
    )
}

/// Forward `derivation_dependency` closure of `roots`; returns every reachable
/// derivation id (roots included).
pub async fn transitive_closure_reachable<C: ConnectionTrait>(
    db: &C,
    roots: &[DerivationId],
) -> Result<HashSet<DerivationId>, DbErr> {
    if roots.is_empty() {
        return Ok(HashSet::new());
    }

    let ids: Vec<uuid::Uuid> = roots.iter().map(|d| d.into_inner()).collect();
    Ok(
        DerivationRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("{} SELECT derivation FROM closure", roots_closure_cte()),
            [ids.into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|r| DerivationId::new(r.derivation))
        .collect(),
    )
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
    let outputs = crate::fetch_in_chunks(drv_ids, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await?;

    let missing_hashes: Vec<String> = outputs
        .iter()
        .filter(|o| o.nar_size.is_none())
        .map(|o| o.hash.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let cached_size_by_hash: HashMap<String, i64> =
        crate::fetch_in_chunks(&missing_hashes, |chunk| async move {
            ECachedPath::find()
                .filter(CCachedPath::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .filter_map(|cp| cp.nar_size.map(|n| (cp.hash, n)))
        .collect();

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

/// Closure size for many roots at once. One recursive statement returns every
/// edge inside the combined closure and a second returns the output sizes, then
/// each root's closure is summed in memory (diamonds deduped via a per-root
/// visited set). Two round trips for the whole batch instead of one full DB walk
/// per root, which matters when a dispatch round backfills many derivations that
/// share most of their closure.
pub async fn transitive_closure_sizes<C: ConnectionTrait>(
    db: &C,
    roots: &[DerivationId],
) -> Result<HashMap<DerivationId, i64>, DbErr> {
    if roots.is_empty() {
        return Ok(HashMap::new());
    }

    let ids: Vec<uuid::Uuid> = roots.iter().map(|d| d.into_inner()).collect();
    let edges = EdgeRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        format!(
            "{} SELECT e.derivation, e.dependency FROM derivation_dependency e \
             JOIN closure c ON e.derivation = c.derivation",
            roots_closure_cte()
        ),
        [ids.into()],
    ))
    .all(db)
    .await?;

    let mut adjacency: HashMap<DerivationId, Vec<DerivationId>> = HashMap::new();
    let mut reachable: HashSet<DerivationId> = roots.iter().copied().collect();
    for edge in edges {
        let (from, to) = (
            DerivationId::new(edge.derivation),
            DerivationId::new(edge.dependency),
        );
        adjacency.entry(from).or_default().push(to);
        reachable.insert(from);
        reachable.insert(to);
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
    use gradient_entity::{derivation_dependency, derivation_output};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn now() -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc()
    }

    fn out(
        derivation: DerivationId,
        hash: &str,
        nar_size: Option<i64>,
    ) -> derivation_output::Model {
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
            derivation,
            dependency,
        }
    }

    // The closure walk projects one `derivation` column; the mock only has to
    // carry that, so the edge model stands in with both ends set to the node.
    fn node(derivation: DerivationId) -> derivation_dependency::Model {
        dep(derivation, derivation)
    }

    #[tokio::test]
    async fn sums_closure_output_sizes() {
        let root = DerivationId::now_v7();
        let child = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // the whole closure walk is now one statement
            .append_query_results([vec![node(root), node(child)]])
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
            // one statement returns every edge inside the closure
            .append_query_results([vec![dep(root, a), dep(root, b), dep(a, c), dep(b, c)]])
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
