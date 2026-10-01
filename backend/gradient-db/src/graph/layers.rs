/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Graph reads over the `derivation_dependency` table: an evaluation's edges and
//! the layering that turns them into the display order a build list is read in.
//! The row `(derivation, dependency)` means "`derivation` depends on
//! `dependency`".

use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use std::collections::{HashMap, HashSet, VecDeque};

use gradient_types::*;

#[derive(FromQueryResult)]
struct EdgeRow {
    derivation: uuid::Uuid,
    dependency: uuid::Uuid,
}

crate::sql! {
    EVAL_DEPENDENCY_EDGES = "SELECT bj.derivation, s.dependency FROM build_job bj, LATERAL (\
           SELECT e.dependency FROM derivation_dependency e \
           WHERE e.derivation = bj.derivation OFFSET 0) s \
         WHERE bj.evaluation = $1",
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(450_000)
            .because("returns every edge of the evaluation: ~3.4 buffers per job, and the \
                      fixture's largest evaluation names ~98k");
}

/// Every `derivation_dependency` edge whose parent derivation is one an
/// `evaluation` holds a `build_job` for. Written as a `LATERAL` probe per
/// parent behind an `OFFSET 0` fence so the planner keeps the nested loop with
/// a per-parent index-only lookup instead of hash-joining the whole edge table
/// (see [`crate::graph::walks`] for why the fence is load-bearing). Measured on
/// the largest production evaluation - 34,778 jobs, 300,534 edges - 140 ms
/// warm against 1,438 ms for the hash join the planner picks unfenced.
pub async fn eval_dependency_edges<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<(DerivationId, DerivationId)>, DbErr> {
    let rows =
        EdgeRow::find_by_statement(EVAL_DEPENDENCY_EDGES.bind([evaluation.into_inner().into()]))
            .all(db)
            .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                DerivationId::new(r.derivation),
                DerivationId::new(r.dependency),
            )
        })
        .collect())
}

/// Longest-path dependency layer of every node in `nodes`, over the
/// `(derivation, dependency)` edges restricted to that set.
///
/// Layer 0 is a node nothing in the set depends on - an entry point, or the
/// root of a closure-scoped view. Every other node sits one layer below its
/// deepest parent, so a derivation always ranks strictly above every
/// derivation it needs, and nodes sharing a layer are genuine siblings that a
/// caller can order however it likes. Edges to or from outside `nodes` are
/// ignored, so a scoped subgraph layers relative to its own root.
pub fn dependency_layers(
    nodes: &HashSet<DerivationId>,
    edges: &[(DerivationId, DerivationId)],
) -> HashMap<DerivationId, u32> {
    let mut dependencies: HashMap<DerivationId, Vec<DerivationId>> = HashMap::new();
    let mut pending_parents: HashMap<DerivationId, usize> = HashMap::new();
    for (derivation, dependency) in edges {
        if !nodes.contains(derivation) || !nodes.contains(dependency) {
            continue;
        }
        dependencies
            .entry(*derivation)
            .or_default()
            .push(*dependency);
        *pending_parents.entry(*dependency).or_default() += 1;
    }

    let mut layers: HashMap<DerivationId, u32> = nodes.iter().map(|n| (*n, 0)).collect();
    let mut frontier: VecDeque<DerivationId> = nodes
        .iter()
        .filter(|n| !pending_parents.contains_key(n))
        .copied()
        .collect();

    while let Some(derivation) = frontier.pop_front() {
        let layer = layers.get(&derivation).copied().unwrap_or(0);
        for dependency in dependencies.get(&derivation).into_iter().flatten() {
            let deepest = layers.entry(*dependency).or_default();
            *deepest = (*deepest).max(layer + 1);
            let remaining = pending_parents.entry(*dependency).or_default();
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                frontier.push_back(*dependency);
            }
        }
    }

    layers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers(
        nodes: &[DerivationId],
        edges: &[(DerivationId, DerivationId)],
    ) -> HashMap<DerivationId, u32> {
        dependency_layers(&nodes.iter().copied().collect(), edges)
    }

    /// The build nothing else needs is the top of the list, and each dependency
    /// sits exactly one layer below the build that pulls it in.
    #[test]
    fn root_is_layer_zero_and_dependencies_descend() {
        let (root, mid, leaf) = (
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
        );

        let out = layers(&[root, mid, leaf], &[(root, mid), (mid, leaf)]);

        assert_eq!(out[&root], 0);
        assert_eq!(out[&mid], 1);
        assert_eq!(out[&leaf], 2);
    }

    /// A diamond shares its sink between two paths of different length. The
    /// longest one wins, so the sink never ranks above the parent that needs
    /// it - the whole point of layering rather than a shortest-path BFS.
    #[test]
    fn diamond_takes_the_longest_path() {
        let (root, short, long_a, sink) = (
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
        );

        let out = layers(
            &[root, short, long_a, sink],
            &[(root, sink), (root, short), (short, long_a), (long_a, sink)],
        );

        assert_eq!(out[&sink], 3);
    }

    /// Several entry points in one evaluation each start their own layer 0.
    #[test]
    fn every_undepended_node_starts_at_zero() {
        let (a, b, shared) = (
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
        );

        let out = layers(&[a, b, shared], &[(a, shared), (b, shared)]);

        assert_eq!((out[&a], out[&b], out[&shared]), (0, 0, 1));
    }

    /// A closure-scoped view layers relative to its own root: edges reaching
    /// derivations outside the set carry no weight.
    #[test]
    fn edges_outside_the_node_set_are_ignored() {
        let (outside, root, dep) = (
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
        );

        let out = layers(
            &[root, dep],
            &[(outside, root), (root, dep), (dep, outside)],
        );

        assert_eq!((out.len(), out[&root], out[&dep]), (2, 0, 1));
    }

    /// A derivation with no edges at all still gets a layer, so the sort key is
    /// total over the evaluation's builds.
    #[test]
    fn isolated_nodes_are_layer_zero() {
        let lonely = DerivationId::now_v7();

        assert_eq!(layers(&[lonely], &[])[&lonely], 0);
    }
}
