/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, EntityTrait,
    IntoActiveModel, QueryFilter, Statement,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::signing::sign_derivation_output;

/// Checks every `is_cached = true` output against the NAR store.
/// Resets `is_cached = false` (and clears file metadata) for any output
/// whose NAR file is no longer present so the cache loop will re-pack it.
pub(super) async fn validate_cached_outputs(state: Arc<ServerState>) -> Result<()> {
    let cached = EDerivationOutput::find()
        .filter(CDerivationOutput::IsCached.eq(true))
        .all(&state.db)
        .await
        .context("Failed to query cached outputs for validation")?;

    let mut reset = 0usize;
    for output in cached {
        match state.nar_storage.get(&output.hash).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                warn!(hash = %output.hash, package = %output.package, "NAR file missing for cached output, resetting is_cached");
                let mut active = output.into_active_model();
                active.is_cached = Set(false);
                active.file_hash = Set(None);
                active.file_size = Set(None);
                active.nar_size = Set(None);
                if let Err(e) = active.update(&state.db).await {
                    error!(error = %e, "Failed to reset is_cached for missing NAR");
                } else {
                    reset += 1;
                }
            }
            Err(e) => {
                warn!(error = %e, hash = %output.hash, "Failed to check NAR file presence");
            }
        }
    }

    if reset > 0 {
        info!(
            count = reset,
            "Reset is_cached for outputs with missing NARs"
        );
    }
    Ok(())
}

/// Signs any `is_cached = true` outputs that are missing a signature for
/// one or more of the organization's active caches. Handles the case where
/// a cache is added after outputs were already packed.
pub(super) async fn sign_missing_signatures(state: Arc<ServerState>) -> Result<()> {
    let cached = EDerivationOutput::find()
        .filter(CDerivationOutput::IsCached.eq(true))
        .all(&state.db)
        .await
        .context("Failed to query cached outputs for signature check")?;

    for output in cached {
        let derivation = match EDerivation::find_by_id(output.derivation)
            .one(&state.db)
            .await?
        {
            Some(d) => d,
            None => continue,
        };

        let cache_ids: Vec<Uuid> = match EOrganizationCache::find()
            .filter(COrganizationCache::Organization.eq(derivation.organization))
            .all(&state.db)
            .await
        {
            Ok(ocs) => ocs.into_iter().map(|oc| oc.cache).collect(),
            Err(_) => continue,
        };

        let active_caches = match ECache::find()
            .filter(CCache::Id.is_in(cache_ids))
            .filter(CCache::Active.eq(true))
            .all(&state.db)
            .await
        {
            Ok(c) => c,
            Err(_) => continue,
        };

        for cache in active_caches {
            let (hash, _) = match core::sources::get_hash_from_path(output.output.clone()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let already_signed = match ECachedPath::find()
                .filter(CCachedPath::Hash.eq(&hash))
                .one(&state.db)
                .await
            {
                Ok(Some(cp)) => ECachedPathSignature::find()
                    .filter(CCachedPathSignature::CachedPath.eq(cp.id))
                    .filter(CCachedPathSignature::Cache.eq(cache.id))
                    .one(&state.db)
                    .await
                    .unwrap_or(None)
                    .and_then(|s| s.signature)
                    .is_some(),
                _ => false,
            };

            if !already_signed {
                sign_derivation_output(Arc::clone(&state), cache, output.clone()).await;
            }
        }
    }

    Ok(())
}

pub async fn cleanup_old_evaluations(state: Arc<ServerState>) -> Result<()> {
    let projects = EProject::find()
        .all(&state.db)
        .await
        .context("Failed to query projects for evaluation GC")?;

    for project in projects {
        let keep = project.keep_evaluations as usize;
        if keep == 0 {
            continue;
        }
        if let Err(e) = core::db::gc_project_evaluations(Arc::clone(&state), project.id, keep).await
        {
            warn!(error = %e, project_id = %project.id, "Evaluation GC failed for project");
        }
    }

    Ok(())
}

/// Cache NAR TTL pass: deletes `cache_derivation` rows whose `last_fetched_at` is older
/// than `nar_ttl_hours`. For each expired row, deletes the NAR file from storage and
/// drops the row. The derivation and its outputs stay (other caches may still hold them).
pub async fn cleanup_stale_cached_nars(state: Arc<ServerState>) -> Result<()> {
    let ttl_hours = state.cli.nar_ttl_hours;
    if ttl_hours == 0 {
        return Ok(());
    }

    let rows = state
        .db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT cd.id, cd.cache, cd.derivation
               FROM cache_derivation cd
               WHERE cd.last_fetched_at IS NOT NULL
                 AND cd.last_fetched_at < NOW() AT TIME ZONE 'UTC' - ($1 * INTERVAL '1 hour')"#,
            [sea_orm::Value::BigInt(Some(ttl_hours as i64))],
        ))
        .await
        .context("Failed to query stale cache_derivation rows")?;

    for row in rows {
        let cd_id: Uuid = match row.try_get("", "id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let drv_id: Uuid = match row.try_get("", "derivation") {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Find the outputs of the derivation; remove their NAR files (if no other
        // cache_derivation row keeps them alive).
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(drv_id))
            .all(&state.db)
            .await
            .unwrap_or_default();

        // Drop the cache_derivation row first; revocation of dependents follows.
        if let Some(cd) = ECacheDerivation::find_by_id(cd_id)
            .one(&state.db)
            .await
            .ok()
            .flatten()
        {
            let _ = cd.into_active_model().delete(&state.db).await;
        }

        // Best-effort: NAR file is shared by every cache for this output, so only
        // delete when no cache_derivation row remains for the derivation.
        let still_held = ECacheDerivation::find()
            .filter(CCacheDerivation::Derivation.eq(drv_id))
            .one(&state.db)
            .await
            .ok()
            .flatten()
            .is_some();
        if !still_held {
            for o in &outputs {
                if let Err(e) = state.nar_storage.delete(&o.hash).await {
                    warn!(error = %e, hash = %o.hash, "Failed to remove stale compressed NAR");
                }
            }
        }
    }

    Ok(())
}

pub async fn cleanup_orphaned_cache_files(state: Arc<ServerState>) -> Result<()> {
    let hashes = state
        .nar_storage
        .list_hashes()
        .await
        .context("Failed to list NAR store")?;

    let mut removed = 0usize;
    for hash in hashes {
        let exists = EDerivationOutput::find()
            .filter(
                Condition::all()
                    .add(CDerivationOutput::Hash.eq(hash.clone()))
                    .add(CDerivationOutput::IsCached.eq(true)),
            )
            .one(&state.db)
            .await
            .context("Failed to check derivation output")?
            .is_some();

        if !exists {
            if let Err(e) = state.nar_storage.delete(&hash).await {
                error!(hash = %hash, error = %e, "Failed to remove orphaned NAR");
            } else {
                debug!(hash = %hash, "Removed orphaned NAR");
                removed += 1;
            }
        }
    }

    if removed > 0 {
        info!(count = removed, "Removed orphaned NAR files");
    }

    Ok(())
}
