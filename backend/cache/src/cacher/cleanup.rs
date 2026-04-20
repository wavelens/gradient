/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use core::types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, EntityTrait,
    IntoActiveModel, QueryFilter, Statement,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

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
        let output_exists = EDerivationOutput::find()
            .filter(
                Condition::all()
                    .add(CDerivationOutput::Hash.eq(hash.clone()))
                    .add(CDerivationOutput::IsCached.eq(true)),
            )
            .one(&state.db)
            .await
            .context("Failed to check derivation output")?
            .is_some();

        // Also keep NARs referenced by a fully-uploaded cached_path row
        // (e.g. `.drv` files that aren't tracked via derivation_output).
        let cached_path_exists = ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash.clone()))
            .filter(CCachedPath::FileHash.is_not_null())
            .one(&state.db)
            .await
            .context("Failed to check cached_path")?
            .is_some();

        let exists = output_exists || cached_path_exists;

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
