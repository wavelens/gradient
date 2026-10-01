/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Best-effort log substitution for substituted / externally-cached builds.
//!
//! A build-once shared build has a single attempt, so there is no sibling to dedup a
//! log from; the only source is the upstream cache's Hydra-style `/log/{drv}`
//! endpoint, fetched from the upstream caches the project's workers substitute from
//! and appended to the shared build's latest attempt log. Every failure is non-fatal:
//! log substitution must never break the build pipeline.

use std::sync::Arc;

use anyhow::Result;
use gradient_core::ServerState;
use gradient_core::upstream_source::{UpstreamSource, fetch_upstream_log};
use gradient_entity::evaluation::Entity as EEvaluation;
use gradient_types::ids::{DerivationBuildId, DerivationId, ProjectId};
use sea_orm::EntityTrait;
use tracing::{debug, warn};

const UPSTREAM_WINDOW_MINUTES: i64 = 60;

/// Append an upstream cache's build log to `derivation_build`'s latest attempt
/// log when it has none yet. Always returns `Ok` - failures are logged, never
/// propagated, so the caller's pipeline is unaffected.
pub async fn substitute_log(
    state: Arc<ServerState>,
    derivation_build: DerivationBuildId,
    derivation_id: DerivationId,
    drv_path: String,
) -> Result<()> {
    let Some(attempt_id) = gradient_db::scheduling::build_attempt::latest_attempt_id(
        &state.worker_db,
        derivation_build,
    )
    .await
    .ok()
    .flatten() else {
        return Ok(());
    };

    let has_log = state
        .log_storage
        .read(attempt_id)
        .await
        .map(|b| !b.is_empty())
        .unwrap_or(false);
    if has_log {
        return Ok(());
    }

    let Some(project_id) = project_for_derivation(&state, derivation_id).await else {
        return Ok(());
    };

    let sources = match gradient_db::caches::upstream::upstream_endpoints_for_project(
        &state.worker_db,
        project_id,
        UPSTREAM_WINDOW_MINUTES,
    )
    .await
    {
        Ok(mut endpoints) => {
            gradient_core::upstream::order_endpoints(&mut endpoints);
            endpoints
                .into_iter()
                .map(|e| UpstreamSource {
                    id: e.id,
                    url: e.url,
                    http1_only: e.http1_only,
                })
                .collect::<Vec<_>>()
        }
        Err(e) => {
            warn!(error = %e, "substitute_log: upstream lookup failed");
            return Ok(());
        }
    };

    let Some(drv_basename) = std::path::Path::new(&drv_path)
        .file_name()
        .and_then(|n| n.to_str())
    else {
        return Ok(());
    };

    match fetch_upstream_log(&sources, drv_basename).await {
        Some(body) => {
            if let Err(e) = state.log_storage.append(attempt_id, &body).await {
                warn!(error = %e, "substitute_log: log_storage.append failed");
            } else if let Err(e) =
                gradient_db::status::enqueue_log_finalize(&state.worker_db, [attempt_id]).await
            {
                warn!(error = %e, "substitute_log: failed to finalize the substituted log");
            }
        }
        None => debug!(drv = drv_basename, "substitute_log: no upstream has a log"),
    }

    Ok(())
}

/// Resolve a project that owns the derivation via any referencing eval.
async fn project_for_derivation(
    state: &Arc<ServerState>,
    derivation: DerivationId,
) -> Option<ProjectId> {
    let jobs =
        gradient_db::graph::reachability::build_jobs_for_derivation(&state.worker_db, derivation)
            .await
            .ok()?;
    for job in jobs {
        if let Ok(Some(eval)) = EEvaluation::find_by_id(job.evaluation)
            .one(&state.worker_db)
            .await
            && let Some(project) = crate::loops::project_id_for_eval(state, &eval).await
        {
            return Some(project);
        }
    }

    None
}
