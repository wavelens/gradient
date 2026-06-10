/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::db::collect_transitive_dependents;
use gradient_core::sources::get_hash_from_path;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::sync::Arc;
use tracing::{info, warn};

/// Invalidates a path's cached state across all caches:
///   - removes its NAR file from storage
///   - clears `is_cached` / `file_hash` / `file_size` on all matching outputs
///   - deletes any `cache_derivation` rows for the owning derivation
///   - walks reverse dependency edges and deletes `cache_derivation` rows for
///     every transitive dependent in the same cache (their closures are now
///     incomplete). Their NAR files stay; only the closure assertion is revoked.
pub async fn invalidate_cache_for_path(state: Arc<ServerState>, path: String) -> Result<()> {
    let (hash, package) = get_hash_from_path(path.clone())
        .with_context(|| format!("Failed to parse path {}", path))?;

    let outputs = EDerivationOutput::find()
        .filter(
            Condition::all()
                .add(CDerivationOutput::Hash.eq(hash.clone()))
                .add(CDerivationOutput::Package.eq(package.clone()))
                .add(CDerivationOutput::IsCached.eq(true)),
        )
        .all(&state.worker_db)
        .await
        .context("Database error while finding derivation outputs")?;

    for output in outputs {
        let derivation_id = output.derivation;

        let mut active = output.clone().into_active_model();
        active.is_cached = Set(false);
        active
            .update(&state.worker_db)
            .await
            .context("Failed to update derivation output")?;

        state
            .nar_storage
            .delete(&hash)
            .await
            .with_context(|| format!("Failed to remove cached NAR for {}", hash))?;

        // Delete cached_path + signatures for this output.
        let cached_paths = ECachedPath::find()
            .filter(CCachedPath::Hash.eq(&hash))
            .all(&state.worker_db)
            .await
            .context("Failed to find cached_path rows")?;

        for cp in &cached_paths {
            let sigs = ECachedPathSignature::find()
                .filter(CCachedPathSignature::CachedPath.eq(cp.id))
                .all(&state.worker_db)
                .await
                .unwrap_or_default();
            for sig in sigs {
                sig.into_active_model()
                    .delete(&state.worker_db)
                    .await
                    .context("Failed to delete signature")?;
            }
            cp.clone()
                .into_active_model()
                .delete(&state.worker_db)
                .await
                .context("Failed to delete cached_path")?;
        }

        // Drop cache_derivation rows for this derivation in every cache,
        // plus walk reverse derivation_dependency edges and remove rows for
        // every dependent (its closure is no longer complete).
        revoke_cache_derivation_closure(&state, derivation_id).await?;

        info!(path = %path, "Invalidated cache for path");
    }

    Ok(())
}

/// Removes all `cache_derivation` rows touching `derivation_id` and any of its
/// transitive dependents across every cache.
async fn revoke_cache_derivation_closure(
    state: &Arc<ServerState>,
    derivation_id: DerivationId,
) -> Result<()> {
    let visited = collect_transitive_dependents(&state.worker_db, derivation_id).await?;
    let drv_ids: Vec<DerivationId> = visited.into_iter().collect();
    let cache_rows = ECacheDerivation::find()
        .filter(CCacheDerivation::Derivation.is_in(drv_ids))
        .all(&state.worker_db)
        .await
        .context("Failed to query cache_derivation rows")?;

    for row in cache_rows {
        let active = row.into_active_model();
        if let Err(e) = active.delete(&state.worker_db).await {
            warn!(error = %e, "Failed to delete cache_derivation row");
        }
    }

    Ok(())
}
