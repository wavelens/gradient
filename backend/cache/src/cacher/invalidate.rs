/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use core::sources::get_hash_from_path;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

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
        .all(&state.db)
        .await
        .context("Database error while finding derivation outputs")?;

    for output in outputs {
        let derivation_id = output.derivation;

        let mut active = output.clone().into_active_model();
        active.is_cached = Set(false);
        active.file_hash = Set(None);
        active.file_size = Set(None);
        active
            .update(&state.db)
            .await
            .context("Failed to update derivation output")?;

        state
            .nar_storage
            .delete(&hash)
            .await
            .with_context(|| format!("Failed to remove cached NAR for {}", hash))?;

        let signatures = EDerivationOutputSignature::find()
            .filter(CDerivationOutputSignature::DerivationOutput.eq(output.id))
            .all(&state.db)
            .await
            .context("Failed to find derivation output signatures")?;

        for signature in signatures {
            let asignature = signature.into_active_model();
            asignature
                .delete(&state.db)
                .await
                .context("Failed to delete signature")?;
        }

        // Drop cache_derivation rows for this derivation in every cache,
        // plus walk reverse derivation_dependency edges and remove rows for
        // every dependent (its closure is no longer complete).
        revoke_cache_derivation_closure(&state, derivation_id).await?;

        info!(path = %path, "Invalidated cache for path");
    }

    Ok(())
}

/// Walks reverse `derivation_dependency` edges starting at `derivation_id` and removes
/// all `cache_derivation` rows touching the visited derivations across every cache.
async fn revoke_cache_derivation_closure(
    state: &Arc<ServerState>,
    derivation_id: Uuid,
) -> Result<()> {
    let mut visited: HashSet<Uuid> = HashSet::new();
    let mut frontier = vec![derivation_id];
    visited.insert(derivation_id);

    while !frontier.is_empty() {
        let edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Dependency.is_in(frontier.clone()))
            .all(&state.db)
            .await
            .context("Failed to walk reverse derivation_dependency")?;
        frontier.clear();
        for edge in edges {
            if visited.insert(edge.derivation) {
                frontier.push(edge.derivation);
            }
        }
    }

    let drv_ids: Vec<Uuid> = visited.into_iter().collect();
    let cache_rows = ECacheDerivation::find()
        .filter(CCacheDerivation::Derivation.is_in(drv_ids))
        .all(&state.db)
        .await
        .context("Failed to query cache_derivation rows")?;

    for row in cache_rows {
        let active = row.into_active_model();
        if let Err(e) = active.delete(&state.db).await {
            warn!(error = %e, "Failed to delete cache_derivation row");
        }
    }

    Ok(())
}
