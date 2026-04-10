/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use chrono::Utc;
use core::sources::get_path_from_derivation_output;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::gcroot::create_gcroot;
use super::signing::{pack_derivation_output, sign_derivation_output};

/// Caches a single derivation output to all caches subscribed by its owning organisation.
///
/// After all outputs of a derivation are `is_cached = true` and the closure is fully
/// cached, the caller (driven from `cache_loop`) records a `cache_derivation` row.
pub async fn cache_derivation_output(state: Arc<ServerState>, output: MDerivationOutput) {
    let derivation = match EDerivation::find_by_id(output.derivation)
        .one(&state.db)
        .await
    {
        Ok(Some(d)) => d,
        Ok(None) => {
            error!("Derivation not found: {}", output.derivation);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query derivation");
            return;
        }
    };

    let organization = match EOrganization::find_by_id(derivation.organization)
        .one(&state.db)
        .await
    {
        Ok(Some(o)) => o,
        Ok(None) => {
            error!("Organization not found: {}", derivation.organization);
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query organization");
            return;
        }
    };

    let path = get_path_from_derivation_output(output.clone());

    // Ensure the path is present locally — for substituted builds it may not
    // have been fetched yet.
    match state.nix_store.query_pathinfo(path.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            // Try to substitute from binary caches before giving up.
            if let Err(e) = state.nix_store.ensure_path(path.clone()).await {
                warn!(error = %e, path = %path, "Path not in local store and substitution failed, skipping cache");
                return;
            }
            // Verify it is now present.
            match state.nix_store.query_pathinfo(path.clone()).await {
                Ok(Some(_)) => {}
                _ => {
                    warn!(path = %path, "Path still not in local store after substitution, skipping cache");
                    return;
                }
            }
        }
        Err(e) => {
            error!(error = %e, path = %path, "Failed to query local store, skipping cache");
            return;
        }
    }

    let cache_ids: Vec<Uuid> = match EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(organization.id))
        .all(&state.db)
        .await
    {
        Ok(ocs) => ocs.into_iter().map(|oc| oc.cache).collect(),
        Err(e) => {
            error!(error = %e, "Failed to query organization caches");
            return;
        }
    };

    let active_caches = match ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .filter(CCache::Active.eq(true))
        .all(&state.db)
        .await
    {
        Ok(caches) => caches,
        Err(e) => {
            error!(error = %e, "Failed to query active caches");
            return;
        }
    };

    for cache in &active_caches {
        sign_derivation_output(Arc::clone(&state), cache.clone(), output.clone()).await;
    }

    info!(
        hash = %output.hash,
        package = %output.package,
        "Caching derivation output"
    );

    let pack_result = pack_derivation_output(Arc::clone(&state), output.clone()).await;

    let (file_hash, file_size, nar_size) = match pack_result {
        Ok(result) => result,
        Err(e) => {
            error!(error = %e, hash = %output.hash, "Failed to pack derivation output: {:#}", e);
            return;
        }
    };

    let mut active = output.clone().into_active_model();
    active.file_hash = Set(Some(file_hash));
    active.file_size = Set(Some(file_size as i64));
    active.nar_size = Set(Some(nar_size as i64));
    info!(
        hash = %output.hash,
        file_size = file_size,
        nar_size = nar_size,
        "Packed and uploaded NAR"
    );
    active.is_cached = Set(true);

    if let Err(e) = active.update(&state.db).await {
        error!(error = %e, "Failed to update derivation output cache status");
        return;
    }

    create_gcroot(&state, &output.hash, &output.package).await;

    // After updating, check whether this derivation's full closure is now
    // available in any of the caches. If so, record the cache_derivation row.
    for cache in &active_caches {
        if let Err(e) =
            try_record_cache_derivation(Arc::clone(&state), cache.id, derivation.id).await
        {
            warn!(error = %e, cache_id = %cache.id, drv_id = %derivation.id,
                "Failed to record cache_derivation");
        }
    }
}

/// If every output of `derivation_id` is cached AND every transitive dependency already
/// has a `cache_derivation` row for `cache_id`, insert the row for this derivation.
async fn try_record_cache_derivation(
    state: Arc<ServerState>,
    cache_id: Uuid,
    derivation_id: Uuid,
) -> Result<()> {
    // 1. All outputs of this derivation cached?
    let any_uncached = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation_id))
        .filter(CDerivationOutput::IsCached.eq(false))
        .one(&state.db)
        .await?
        .is_some();
    if any_uncached {
        return Ok(());
    }

    // 2. Every direct dependency has a cache_derivation row for this cache.
    let dep_edges = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(derivation_id))
        .all(&state.db)
        .await?;
    for edge in dep_edges {
        let present = ECacheDerivation::find()
            .filter(CCacheDerivation::Cache.eq(cache_id))
            .filter(CCacheDerivation::Derivation.eq(edge.dependency))
            .one(&state.db)
            .await?
            .is_some();
        if !present {
            return Ok(());
        }
    }

    // 3. Already recorded?
    let already = ECacheDerivation::find()
        .filter(CCacheDerivation::Cache.eq(cache_id))
        .filter(CCacheDerivation::Derivation.eq(derivation_id))
        .one(&state.db)
        .await?
        .is_some();
    if already {
        return Ok(());
    }

    let row = ACacheDerivation {
        id: Set(Uuid::new_v4()),
        cache: Set(cache_id),
        derivation: Set(derivation_id),
        cached_at: Set(Utc::now().naive_utc()),
        last_fetched_at: Set(None),
    };
    row.insert(&state.db).await?;
    tracing::debug!(cache_id = %cache_id, derivation_id = %derivation_id, "Recorded cache_derivation");
    Ok(())
}
