/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Self-heal for `BuildFailureKind::InputsUnavailable`: purge stale cache
//! artifacts for reported-missing inputs and re-queue their producers.

use std::sync::Arc;

use anyhow::Result;

use gradient_core::ServerState;
use gradient_types::*;
use tracing::{info, warn};

/// Extract the 32-char hash from a `/nix/store/<hash>-<name>` store path.
fn store_path_hash(store_path: &str) -> Option<&str> {
    store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
        .filter(|h| !h.is_empty())
}

/// Whether any of `derivations` is reachable (has a `build_job`, so promotion can
/// schedule it). A producer with none is an orphan: pruned out of the build graph
/// because a referrer was cached without its closure, it can never be queued and
/// must instead be revived by re-walking its cached referrers.
async fn any_reachable<C: sea_orm::ConnectionTrait>(db: &C, derivations: &[DerivationId]) -> bool {
    for d in derivations {
        if gradient_db::derivation_is_reachable(db, *d)
            .await
            .unwrap_or(false)
        {
            return true;
        }
    }

    false
}

/// Self-heal for `BuildFailureKind::InputsUnavailable`. A build attempt
/// proved these input paths are unfetchable from the cache, so purge each
/// one's stale cache artifact (delete the `cached_path` row + the NAR object,
/// clear the output's `is_cached` / `cached_path`) and reset its producer to
/// `Created`, leaving the derivation graph intact. The producer then rebuilds
/// in-eval and the failed build - marked `FailedTransient`, not permanent -
/// retries once the input is back (the dispatch gate holds it until then).
///
/// A missing input with no producing derivation (a `.drv` file or a source
/// path) has its stale row + object purged just the same; the next eval
/// re-instantiates the `.drv` and re-pushes it, so this is a recoverable
/// purge, not a dead-end.
pub(super) async fn reconcile_missing_inputs(
    state: &Arc<ServerState>,
    failed_derivation: DerivationId,
    missing_paths: &[String],
) -> Result<()> {
    let db = &state.worker_db;
    let mut purged = 0usize;
    let mut referrers_demoted = 0usize;
    let mut sources_purged: Vec<&str> = Vec::new();
    let mut demoted_producers: Vec<DerivationId> = Vec::new();
    // Set when a missing input cannot be reached upward (no producer row and
    // no indexed referrer, or an orphan producer with no indexed referrer):
    // an absent orphan pruned out of the graph. Recovered after the loop by
    // demoting the failed build's cached deps so the next eval re-walks them.
    let mut needs_dep_rewalk = false;
    for path in missing_paths {
        let Some(hash) = store_path_hash(path) else {
            continue;
        };

        // Diagnostic: record why the worker found this input unfetchable
        // even though dispatch treated its producer as done. `fully_cached`
        // true means the DB claimed a complete NAR (stale cached_path / lost
        // object); the producer statuses show whether it was trusted
        // `Substituted` or really `Completed`. The eval arg is unused by the
        // global diagnosis query.
        match gradient_db::diagnose_missing_input(db, EvaluationId::now_v7(), hash).await {
            Ok(d) => warn!(
                %path,
                hash,
                cached_path_present = d.cached_path_present,
                fully_cached = d.fully_cached,
                outputs_cached = d.outputs_cached,
                outputs_total = d.outputs_total,
                producer_statuses = ?d.producer_build_statuses,
                "missing input: cache/build state at failure"
            ),
            Err(e) => warn!(%path, error = %e, "missing input: diagnosis query failed"),
        }

        match gradient_db::demote_cached_output(db, &state.nar_storage, hash).await {
            Ok(drvs) if !drvs.is_empty() => {
                purged += 1;
                // The leaf rebuilds + re-pushes closure-complete; meanwhile drop
                // the now-stale `closure_complete` up the chain so the dispatch
                // gate re-blocks dependents until the closure is whole again.
                if let Err(e) = gradient_db::clear_closure_complete_for_referrers(db, hash).await {
                    warn!(%path, error = %e, "reconcile: clear closure_complete failed");
                }

                // If the producer is an orphan (no `build_job`, so promotion can
                // never queue it - it was pruned out of the build graph because
                // some referrer's output was cached without its closure), the
                // gentle flag clear is not enough: the referrer stays cached,
                // stays pruned, and the orphan is never re-walked. Demote the
                // referrers so the next eval re-walks them, re-records the edge,
                // and schedules the producer.
                let orphan = !any_reachable(db, &drvs).await;
                demoted_producers.extend(drvs);
                if orphan {
                    match gradient_db::demote_referrers_of(db, &state.nar_storage, hash).await {
                        Ok(refs) if !refs.is_empty() => {
                            referrers_demoted += refs.len();
                            demoted_producers.extend(refs);
                        }
                        Ok(_) => needs_dep_rewalk = true,
                        Err(e) => {
                            warn!(%path, error = %e, "reconcile: demote referrers (orphan producer) failed")
                        }
                    }
                }
            }
            Ok(_) => {
                sources_purged.push(path);
                // No producing derivation (a source / `.drv`): it only returns
                // to the cache as part of a referrer's closure, so demote the
                // referrers - a pruned/substituted parent would otherwise never
                // re-push it, stranding dependents forever.
                match gradient_db::demote_referrers_of(db, &state.nar_storage, hash).await {
                    Ok(drvs) if !drvs.is_empty() => {
                        referrers_demoted += drvs.len();
                        demoted_producers.extend(drvs);
                    }
                    Ok(_) => needs_dep_rewalk = true,
                    Err(e) => warn!(%path, error = %e, "reconcile: demote referrers failed"),
                }
            }
            Err(e) => warn!(%path, error = %e, "reconcile: purge cached output failed"),
        }
    }

    // Absent orphan: a missing input with no producer row and no indexed
    // referrer cannot be reached upward, so reach it downward from the failing
    // build - demote its output-only-cached direct deps to force the next eval
    // to re-walk them and re-record the orphan (and its now-buildable subtree).
    if needs_dep_rewalk {
        match gradient_db::demote_output_only_cached_deps(db, &state.nar_storage, failed_derivation)
            .await
        {
            Ok(drvs) => {
                referrers_demoted += drvs.len();
                demoted_producers.extend(&drvs);
                info!(
                    %failed_derivation,
                    count = drvs.len(),
                    "reconcile: demoted output-only-cached direct deps to re-walk an absent orphan input"
                );
            }
            Err(e) => {
                warn!(%failed_derivation, error = %e, "reconcile: demote output-only-cached deps failed")
            }
        }
    }

    // A demanded output whose producer is terminal-*failed* (not 3/7, which
    // `demote_cached_output` already reset) must retry: the dependent that just
    // failed is a fresh build intent, and waiting for an eval to requeue it
    // dead-ends whenever evals are aborted. Re-queue on demand, decoupled from
    // eval completion - `requeue_failed_anchors` only touches statuses 4/5/6/9.
    let requeued = if demoted_producers.is_empty() {
        0
    } else {
        gradient_db::requeue_failed_anchors(db, &demoted_producers)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "reconcile: requeue failed producers failed");
                0
            })
    };

    if !sources_purged.is_empty() {
        info!(
            %failed_derivation,
            count = sources_purged.len(),
            sample = ?sources_purged.iter().take(5).collect::<Vec<_>>(),
            "reconcile: purged stale cache rows + objects for inputs with no producing \
             derivation (.drv / source); the next evaluation re-instantiates and re-pushes them"
        );
    }

    info!(
        %failed_derivation,
        purged,
        sources_purged = sources_purged.len(),
        referrers_demoted,
        requeued,
        paths = missing_paths.len(),
        "reconciled missing inputs; stale cache rows + objects purged for next-eval rebuild"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::store_path_hash;

    #[test]
    fn store_path_hash_extracts_32_char_hash() {
        assert_eq!(
            store_path_hash("/nix/store/g9y0fvqh2c991vjprgz9mvdm0zj7ggij-python3-static-3.13"),
            Some("g9y0fvqh2c991vjprgz9mvdm0zj7ggij")
        );
        assert_eq!(store_path_hash("not-a-store-path"), None);
        assert_eq!(store_path_hash("/nix/store/"), None);
    }
}
