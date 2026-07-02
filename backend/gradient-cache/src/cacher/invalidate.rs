/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_db::collect_transitive_dependents;
use gradient_sources::get_hash_from_path;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait};
use std::sync::Arc;
use tracing::info;

/// Invalidates a path's cached state across all caches, in one transaction, by
/// demoting it like any other proven-bad artifact ([`gradient_db::demote_cached_output`]):
///   - clears the cache link AND upstream availability on all matching outputs
///   - resets trusted producers (`Completed`/`Substituted`) to `Created` so the
///     path rebuilds instead of staying trusted-but-gone
///   - deletes the `cached_path` rows (signatures cascade) and the NAR object
///   - clears the gate flags the deleted rows backed
///   - revokes `cache_derivation` closure assertions for the producers and
///     every transitive dependent (their closures are no longer complete);
///     dependents' NAR files stay.
pub async fn invalidate_cache_for_path(state: Arc<ServerState>, path: String) -> Result<()> {
    let (hash, _package) = get_hash_from_path(path.clone())
        .with_context(|| format!("Failed to parse path {}", path))?;

    let txn = state
        .worker_db
        .inner()
        .begin()
        .await
        .context("Failed to open invalidation transaction")?;

    let producers = gradient_db::demote_cached_output(&txn, &state.nar_storage, &hash)
        .await
        .context("Failed to demote cached output")?;
    gradient_db::clear_gate_flags_for_hashes(&txn, std::slice::from_ref(&hash))
        .await
        .context("Failed to clear gate flags")?;
    gradient_db::clear_closure_complete_for_referrers(&txn, &hash)
        .await
        .context("Failed to clear referrer closure flags")?;

    for derivation_id in &producers {
        revoke_cache_derivation_closure(&txn, *derivation_id).await?;
    }

    txn.commit()
        .await
        .context("Failed to commit invalidation")?;

    info!(path = %path, producers = producers.len(), "Invalidated cache for path");
    Ok(())
}

/// Removes all `cache_derivation` rows touching `derivation_id` and any of its
/// transitive dependents across every cache.
async fn revoke_cache_derivation_closure<C: ConnectionTrait>(
    db: &C,
    derivation_id: DerivationId,
) -> Result<()> {
    let visited = collect_transitive_dependents(db, derivation_id).await?;
    let drv_ids: Vec<DerivationId> = visited.into_iter().collect();
    gradient_db::for_each_chunk(&drv_ids, |chunk| async move {
        ECacheDerivation::delete_many()
            .filter(CCacheDerivation::Derivation.is_in(chunk))
            .exec(db)
            .await
    })
    .await
    .context("Failed to delete cache_derivation rows")?;

    Ok(())
}
