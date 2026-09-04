/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Which derivations an evaluation walk may prune, answered after every queued write.

use std::collections::{HashMap, HashSet};

use gradient_db::WorkerDb;
use gradient_types::ids::DerivationId;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

/// The prunable-derivations lookup. Any error propagates: the caller prunes nothing.
pub(crate) async fn prunable(
    db: &WorkerDb,
    drv_hashes: Vec<String>,
) -> Result<Vec<String>, sea_orm::DbErr> {
    let candidates = EDerivation::find()
        .filter(CDerivation::Hash.is_in(drv_hashes))
        .all(db)
        .await?;
    if candidates.is_empty() {
        return Ok(vec![]);
    }

    let drv_ids: Vec<DerivationId> = candidates.iter().map(|d| d.id).collect();
    let outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(drv_ids.clone()))
        .all(db)
        .await?;

    let anchors = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.is_in(drv_ids))
        .all(db)
        .await?;
    let unresolved: HashSet<DerivationId> = anchors
        .iter()
        .filter(|b| b.edges_unresolved)
        .map(|b| b.derivation)
        .collect();
    // Local-prune precondition: the anchor succeeded and its subtree's edges are
    // durably recorded, so skipping the walk loses nothing the graph needs.
    let complete_anchors: HashSet<DerivationId> = anchors
        .iter()
        .filter(|b| b.edges_complete && b.status.is_terminal_success())
        .map(|b| b.derivation)
        .collect();

    let out_hashes: Vec<String> = outputs
        .iter()
        .map(|o| o.hash.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let closure_cached: HashSet<String> = ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(out_hashes))
        .all(db)
        .await?
        .into_iter()
        .filter(|cp| cp.is_fully_cached() && cp.closure_complete)
        .map(|cp| cp.hash)
        .collect();

    let candidates: Vec<(DerivationId, String)> = candidates
        .into_iter()
        .map(|d| (d.id, d.store_path()))
        .collect();

    Ok(prunable_known_derivations(
        candidates,
        &outputs,
        &unresolved,
        &complete_anchors,
        &closure_cached,
    ))
}

/// Decide which `(derivation_id, store_path)` candidates the eval BFS may prune.
///
/// Upstream arm: every output is on a real upstream (`external_url`), which
/// serves a complete closure, so a build worker fetches the pruned subtree on
/// demand. Local arm: the anchor is terminal-success with `edges_complete` and
/// every output has a fully-cached `cached_path` with `closure_complete`; bare
/// `is_cached` is not enough, because our own cache is populated output-only and
/// pruning on it stranded never-pushed closure members as permanent
/// `InputsUnavailable` dead-ends. An `edges_unresolved` anchor is never prunable:
/// only a re-walk rediscovers its dropped edge and clears the flag.
fn prunable_known_derivations(
    candidates: Vec<(DerivationId, String)>,
    outputs: &[MDerivationOutput],
    unresolved: &HashSet<DerivationId>,
    complete_anchors: &HashSet<DerivationId>,
    closure_cached: &HashSet<String>,
) -> Vec<String> {
    let mut counts: HashMap<DerivationId, (usize, usize, usize)> = HashMap::new();
    for o in outputs {
        let entry = counts.entry(o.derivation).or_insert((0, 0, 0));
        entry.0 += 1;
        if o.external_url.is_none() {
            entry.1 += 1;
        }
        if !closure_cached.contains(&o.hash) {
            entry.2 += 1;
        }
    }

    candidates
        .into_iter()
        .filter(|(id, _)| {
            let (total, off_upstream, off_local) = counts.get(id).copied().unwrap_or((0, 0, 0));
            let upstream_ok = off_upstream == 0;
            let local_ok = off_local == 0 && complete_anchors.contains(id);
            total > 0 && !unresolved.contains(id) && (upstream_ok || local_ok)
        })
        .map(|(_, store_path)| store_path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::prunable_known_derivations;
    use gradient_types::MDerivationOutput;
    use gradient_types::ids::{DerivationId, DerivationOutputId};
    use std::collections::HashSet;

    fn output(drv: DerivationId, hash: &str) -> MDerivationOutput {
        MDerivationOutput {
            id: DerivationOutputId::now_v7(),
            derivation: drv,
            hash: hash.to_string(),
            ..Default::default()
        }
    }

    fn prune(
        candidates: Vec<(DerivationId, String)>,
        outputs: &[MDerivationOutput],
        unresolved: &HashSet<DerivationId>,
        complete_anchors: &HashSet<DerivationId>,
        closure_cached: &HashSet<String>,
    ) -> Vec<String> {
        prunable_known_derivations(
            candidates,
            outputs,
            unresolved,
            complete_anchors,
            closure_cached,
        )
    }

    #[test]
    fn prunes_only_outputs_on_a_real_upstream() {
        let local = DerivationId::now_v7(); // is_cached in our cache, NOT upstream
        let upstream = DerivationId::now_v7(); // every output on an upstream
        let partial = DerivationId::now_v7(); // one output upstream, one not
        let output_less = DerivationId::now_v7(); // recorded drv, no outputs
        let unknown = DerivationId::now_v7(); // no rows at all

        let mut o_local = output(local, "aaa");
        o_local.is_cached = true;
        let mut o_upstream = output(upstream, "bbb");
        o_upstream.external_url = Some("https://cache.example/bbb.narinfo".to_string());
        let mut o_partial_a = output(partial, "ddd");
        o_partial_a.external_url = Some("https://cache.example/ddd.narinfo".to_string());
        let mut o_partial_b = output(partial, "eee");
        o_partial_b.is_cached = true;

        let outputs = vec![o_local, o_upstream, o_partial_a, o_partial_b];

        let candidates = vec![
            (local, "/nix/store/aaa-local".to_string()),
            (upstream, "/nix/store/bbb-upstream".to_string()),
            (partial, "/nix/store/ddd-partial".to_string()),
            (output_less, "/nix/store/fff-output-less".to_string()),
            (unknown, "/nix/store/ggg-unknown".to_string()),
        ];

        let prunable = prune(
            candidates,
            &outputs,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );

        assert_eq!(prunable, vec!["/nix/store/bbb-upstream".to_string()]);
    }

    /// Both local-arm preconditions are load-bearing: a closure-complete output
    /// without the recorded-graph anchor, or a complete anchor with one output
    /// lacking `closure_complete`, must keep walking.
    #[test]
    fn locally_closure_complete_anchor_prunes() {
        let complete = DerivationId::now_v7(); // anchor complete + output closure-cached
        let no_anchor = DerivationId::now_v7(); // output closure-cached, no complete anchor
        let half_cached = DerivationId::now_v7(); // anchor complete, one output not closure-cached

        let outputs = vec![
            output(complete, "aaa"),
            output(no_anchor, "bbb"),
            output(half_cached, "ccc"),
            output(half_cached, "ddd"),
        ];
        let candidates = vec![
            (complete, "/nix/store/aaa-complete".to_string()),
            (no_anchor, "/nix/store/bbb-no-anchor".to_string()),
            (half_cached, "/nix/store/ccc-half".to_string()),
        ];
        let complete_anchors = HashSet::from([complete, half_cached]);
        let closure_cached: HashSet<String> = ["aaa", "bbb", "ccc"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let prunable = prune(
            candidates,
            &outputs,
            &HashSet::new(),
            &complete_anchors,
            &closure_cached,
        );

        assert_eq!(prunable, vec!["/nix/store/aaa-complete".to_string()]);
    }

    /// Pruning an `edges_unresolved` anchor skips the re-walk that rediscovers its
    /// dropped edge, leaving it and its dependents stranded off promotion forever.
    #[test]
    fn edges_unresolved_anchor_is_never_prunable() {
        let upstream = DerivationId::now_v7();
        let mut o = output(upstream, "bbb");
        o.external_url = Some("https://cache.example/bbb.narinfo".to_string());
        let candidates = vec![(upstream, "/nix/store/bbb-upstream".to_string())];
        let unresolved = HashSet::from([upstream]);
        let complete_anchors = HashSet::from([upstream]);
        let closure_cached: HashSet<String> = HashSet::from(["bbb".to_string()]);

        assert!(
            prune(
                candidates,
                &[o],
                &unresolved,
                &complete_anchors,
                &closure_cached
            )
            .is_empty(),
            "an edges_unresolved anchor must be re-walked, not pruned"
        );
    }
}
