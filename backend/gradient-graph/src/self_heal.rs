/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_types::*;
use tracing::{info, warn};

fn store_path_hash(store_path: &str) -> Option<&str> {
    store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
        .filter(|h| !h.is_empty())
}

async fn any_reachable<C: sea_orm::ConnectionTrait>(db: &C, derivations: &[DerivationId]) -> bool {
    for d in derivations {
        if gradient_db::graph::reachability::derivation_is_reachable(db, *d)
            .await
            .unwrap_or(false)
        {
            return true;
        }
    }

    false
}

/// The failed build is `FailedTransient` and is retried by `requeue::transient_retries`.
/// That requeue's settle is holding it out of the queue until the input is back.
/// `demote_cached_output` is keeping a still-present input that has no producer.
/// Nothing can rebuild it, and deleting the only copy would fail every parent forever.
pub(crate) async fn repair_missing_inputs(
    ctx: &DbContext,
    failed_derivation: DerivationId,
    missing_paths: &[String],
) -> Result<()> {
    let db = &ctx.worker_db;
    let mut purged = 0usize;
    let mut wanted_by_demoted = 0usize;
    let mut sources_purged: Vec<&str> = Vec::new();
    let mut demoted_producers: Vec<DerivationId> = Vec::new();
    let mut needs_dep_rewalk = false;
    for path in missing_paths {
        let Some(hash) = store_path_hash(path) else {
            continue;
        };

        match gradient_db::caches::demotion::diagnose_missing_input(
            db,
            EvaluationId::now_v7(),
            hash,
        )
        .await
        {
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

        match gradient_db::caches::demotion::demote_cached_output(ctx, hash).await {
            Ok(drvs) if !drvs.is_empty() => {
                purged += 1;
                let orphan = !any_reachable(db, &drvs).await;
                demoted_producers.extend(drvs);
                if orphan {
                    match gradient_db::caches::demotion::demote_parents_of(ctx, hash).await {
                        Ok(refs) if !refs.is_empty() => {
                            wanted_by_demoted += refs.len();
                            match gradient_db::graph::can_start::unwalk_derivations(ctx, &refs)
                                .await
                            {
                                Ok(changes) => {
                                    gradient_db::status::emit_transition_effects(ctx, &changes)
                                        .await?
                                }
                                Err(e) => {
                                    warn!(%path, error = %e, "repair: re-walk parents (orphan producer) failed")
                                }
                            }

                            demoted_producers.extend(refs);
                        }
                        Ok(_) => needs_dep_rewalk = true,
                        Err(e) => {
                            warn!(%path, error = %e, "repair: demote parents (orphan producer) failed")
                        }
                    }
                }
            }
            Ok(_) => {
                sources_purged.push(path);
                match gradient_db::caches::demotion::demote_parents_of(ctx, hash).await {
                    Ok(drvs) if !drvs.is_empty() => {
                        wanted_by_demoted += drvs.len();
                        demoted_producers.extend(drvs);
                    }
                    Ok(_) => needs_dep_rewalk = true,
                    Err(e) => warn!(%path, error = %e, "repair: demote parents failed"),
                }
            }
            Err(e) => warn!(%path, error = %e, "repair: purge cached output failed"),
        }
    }

    if needs_dep_rewalk {
        match gradient_db::caches::demotion::demote_output_only_cached_deps(ctx, failed_derivation)
            .await
        {
            Ok(drvs) => {
                wanted_by_demoted += drvs.len();
                demoted_producers.extend(&drvs);
                info!(
                    %failed_derivation,
                    count = drvs.len(),
                    "repair: demoted output-only-cached direct deps to re-walk an absent orphan input"
                );
            }
            Err(e) => {
                warn!(%failed_derivation, error = %e, "repair: demote output-only-cached deps failed")
            }
        }
    }

    // A wanted output with a terminal-failed producer must retry right away.
    // Waiting for an eval to requeue it can dead-end whenever evals are aborted.
    let requeued = if demoted_producers.is_empty() {
        0
    } else {
        match gradient_db::graph::promotion::requeue_failed_shared_builds(db, &demoted_producers)
            .await
        {
            Ok(changes) => {
                let thawed = changes.len();
                gradient_db::status::emit_transition_effects(ctx, &changes).await?;
                thawed
            }
            Err(e) => {
                warn!(error = %e, "repair: requeue failed producers failed");
                0
            }
        }
    };

    if !sources_purged.is_empty() {
        info!(
            %failed_derivation,
            count = sources_purged.len(),
            sample = ?sources_purged.iter().take(5).collect::<Vec<_>>(),
            "repair: purged stale cache rows + objects for inputs with no producing \
             derivation (.drv / source); the next evaluation re-instantiates and re-pushes them"
        );
    }

    info!(
        %failed_derivation,
        purged,
        sources_purged = sources_purged.len(),
        wanted_by_demoted,
        requeued,
        paths = missing_paths.len(),
        "repaired missing inputs; stale cache rows + objects purged for next-eval rebuild"
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
