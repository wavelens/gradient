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

/// Present inputs stay, whoever reported them missing: a worker view of the cache can lag behind
/// storage. Retire the `cached_path` row of a gone object. An immediate retry for its
/// terminal-failed producer.
pub(crate) async fn repair_missing_inputs(
    ctx: &DbContext,
    failed_derivation: DerivationId,
    missing_paths: &[String],
) -> Result<()> {
    use gradient_db::caches::demotion::{DemoteWhen, demote_cached_output, unwalk_parents_of};

    let db = &ctx.worker_db;
    let mut purged = 0usize;
    let mut parents_unwalked = 0usize;
    let mut kept: Vec<&str> = Vec::new();
    let mut demoted_producers: Vec<DerivationId> = Vec::new();
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

        match demote_cached_output(ctx, hash, DemoteWhen::ObjectMissing).await {
            Ok(drvs) if !drvs.is_empty() => {
                purged += 1;
                if !any_reachable(db, &drvs).await {
                    match unwalk_parents_of(ctx, hash).await {
                        Ok(parents) => parents_unwalked += parents.len(),
                        Err(e) => {
                            warn!(%path, error = %e, "repair: re-walk of the orphan producer's parents failed")
                        }
                    }
                }
                demoted_producers.extend(drvs);
            }
            Ok(_) => kept.push(path),
            Err(e) => warn!(%path, error = %e, "repair: purge cached output failed"),
        }
    }

    // Leave the derivation walked. Once the evaluation has left its walk phase nothing can walk
    // it again and every consumer of an unwalked row cannot start. The retry loop cannot
    // outgrow `max_attempts`.

    // An immediate retry for a wanted output with a terminal-failed producer: an aborted eval
    // can skip the requeue.
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

    info!(
        %failed_derivation,
        purged,
        kept = kept.len(),
        sample = ?kept.iter().take(5).collect::<Vec<_>>(),
        parents_unwalked,
        requeued,
        paths = missing_paths.len(),
        "repaired missing inputs: gone objects purged, present and producerless paths kept"
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
