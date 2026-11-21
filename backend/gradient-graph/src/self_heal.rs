/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::caches::demotion::{
    StoreContentToRetire, diagnose_missing_input, object_exists_in_storage_or_error,
};
use gradient_db::store_path_content_variant_reader_choice::list_all_content_variant_rows_for_store_path_hashes;
use gradient_types::*;
use tracing::{info, warn};

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

async fn stored_content_with_missing_object(
    ctx: &DbContext,
    path: &str,
    hash: &str,
) -> Option<StoreContentToRetire> {
    let variants = match list_all_content_variant_rows_for_store_path_hashes(
        &ctx.worker_db,
        &[hash.to_owned()],
    )
    .await
    {
        Ok(variants) => variants,
        Err(e) => {
            warn!(%path, error = %e, "missing input: content variant query failed");
            return None;
        }
    };
    if variants.is_empty() {
        warn!(%path, hash, "missing input: no stored content variant");
        return Some(StoreContentToRetire::StoreHashesWithGraphOutput(vec![
            hash.to_owned(),
        ]));
    }

    let mut missing = Vec::new();
    for variant in &variants {
        let object_present = object_exists_in_storage_or_error(ctx, variant).await;
        match diagnose_missing_input(&ctx.worker_db, variant).await {
            Ok(d) => warn!(
                %path,
                content_variant = %d.content_variant,
                nar_object_storage_key = %d.nar_object_storage_key,
                object_present,
                outputs_total = d.outputs_total,
                producer_statuses = ?d.producer_build_statuses,
                "missing input: cache/build state at failure"
            ),
            Err(e) => warn!(%path, error = %e, "missing input: diagnosis query failed"),
        }
        if !object_present {
            missing.push(variant.id);
        }
    }

    (!missing.is_empty()).then_some(StoreContentToRetire::OnlyTheseCachedPathIds(missing))
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
        let Ok(stored) = StorePath::parse_stored(path) else {
            continue;
        };
        let hash = stored.hash();
        let Some(content) = stored_content_with_missing_object(ctx, path, hash).await else {
            kept.push(path);
            continue;
        };

        match demote_cached_output(ctx, &content, DemoteWhen::ObjectMissing, false)
            .await
            .map(|demoted| demoted.producers)
        {
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
                let thawed: Vec<_> = changes.iter().map(|c| c.derivation).collect();
                gradient_db::status::emit_transition_effects(ctx, &changes).await?;
                let settled = gradient_db::graph::can_start::settle_created(db, &thawed).await?;
                gradient_db::status::emit_transition_effects(ctx, &settled).await?;
                thawed.len()
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
        "Repaired missing inputs. Purged gone outputs. Marked derivations that list gone paths as unwalked. Kept present objects."
    );
    Ok(())
}
