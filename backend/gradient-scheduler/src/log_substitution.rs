/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Best-effort log substitution for substituted / externally-cached builds.
//!
//! A build-once anchor has a single attempt, so there is no sibling to dedup a
//! log from; the only source is the upstream cache's Hydra-style `/log/{drv}`
//! endpoint, fetched and appended to the anchor's latest attempt log. Every
//! failure is non-fatal: log substitution must never break the build pipeline.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use gradient_core::ServerState;
use gradient_entity::evaluation::Entity as EEvaluation;
use gradient_types::ids::{DerivationBuildId, DerivationId, OrganizationId};
use sea_orm::EntityTrait;
use tracing::{debug, warn};

const LOG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_FETCH_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Append an upstream cache's build log to `derivation_build`'s latest attempt
/// log when it has none yet. Always returns `Ok` - failures are logged, never
/// propagated, so the caller's pipeline is unaffected.
pub async fn substitute_log(
    state: Arc<ServerState>,
    derivation_build: DerivationBuildId,
    derivation_id: DerivationId,
    drv_path: String,
    allow_upstream_fetch: bool,
) -> Result<()> {
    let Some(attempt_id) = gradient_db::latest_attempt_id(&state.worker_db, derivation_build)
        .await
        .ok()
        .flatten()
    else {
        return Ok(());
    };

    let has_log = state
        .log_storage
        .read(attempt_id)
        .await
        .map(|b| !b.is_empty())
        .unwrap_or(false);
    if has_log || !allow_upstream_fetch {
        return Ok(());
    }

    let Some(org_id) = org_for_derivation(&state, derivation_id).await else {
        return Ok(());
    };

    let upstream_urls = match gradient_db::upstream_urls_for_org(&state.worker_db, org_id).await {
        Ok(urls) if !urls.is_empty() => urls,
        Ok(_) => return Ok(()),
        Err(e) => {
            warn!(error = %e, "substitute_log: upstream URL lookup failed");
            return Ok(());
        }
    };

    let Some(drv_basename) = std::path::Path::new(&drv_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
    else {
        return Ok(());
    };

    for upstream in upstream_urls {
        let url = format!("{}/log/{}", upstream.trim_end_matches('/'), drv_basename);
        match fetch_log_body(&state.http, &url).await {
            Ok(Some(body)) => {
                if let Err(e) = state.log_storage.append(attempt_id, &body).await {
                    warn!(error = %e, "substitute_log: log_storage.append failed");
                }

                return Ok(());
            }
            Ok(None) => debug!(%url, "substitute_log: upstream returned no usable body"),
            Err(e) => debug!(%url, error = %e, "substitute_log: upstream fetch failed"),
        }
    }

    Ok(())
}

/// Resolve an organization that owns the derivation via any referencing eval.
async fn org_for_derivation(
    state: &Arc<ServerState>,
    derivation: DerivationId,
) -> Option<OrganizationId> {
    let jobs = gradient_db::build_jobs_for_derivation(&state.worker_db, derivation)
        .await
        .ok()?;
    for job in jobs {
        if let Ok(Some(eval)) = EEvaluation::find_by_id(job.evaluation).one(&state.worker_db).await
            && let Some(org) = crate::dispatch::organization_id_for_eval(state, &eval).await
        {
            return Some(org);
        }
    }

    None
}

async fn fetch_log_body(http: &reqwest::Client, url: &str) -> anyhow::Result<Option<String>> {
    let resp = http.get(url).timeout(LOG_FETCH_TIMEOUT).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }

    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let room = LOG_FETCH_MAX_BYTES.saturating_sub(bytes.len());
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Ok(None);
    }

    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        body.push_str("\n[truncated]\n");
    }

    Ok(Some(body))
}
