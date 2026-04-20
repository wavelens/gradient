/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background scanner that dispatches [`SignJob`]s when `cached_path` rows
//! are missing `cached_path_signature` entries for one or more of the
//! owning org's caches.
//!
//! Runs as a detached tokio task; polls the DB hourly, groups candidate
//! paths by org, and enqueues one `PendingSignJob` per batch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gradient_core::types::proto::{SignItem, SignJob};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::Scheduler;
use crate::jobs::PendingSignJob;

/// How often the scanner sweeps the DB for unsigned `cached_path` rows.
const SCAN_INTERVAL: Duration = Duration::from_secs(3600);

/// Maximum number of items packed into a single `SignJob`. Kept small
/// enough to stay well below the WebSocket frame limit (individual items
/// are tiny — one store path + metadata — but N hundred already pushes
/// multi-kilobyte payloads).
const MAX_ITEMS_PER_JOB: usize = 500;

/// Spawn the sign-job scanner as a detached background task.
pub fn start_sign_scanner(scheduler: Arc<Scheduler>) {
    tokio::spawn(async move { run_loop(scheduler).await });
}

async fn run_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(SCAN_INTERVAL);
    info!("sign scanner loop started");
    loop {
        interval.tick().await;
        if let Err(e) = run_one_pass(&scheduler).await {
            error!(error = %e, "sign scanner pass failed");
        }
    }
}

/// Single scan pass — enqueues at most one job per org. Exposed for tests
/// and for callers that want to force a pass.
pub async fn run_one_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
    let state = &scheduler.state;

    // For each org, find cached_path rows that:
    //  - have full metadata populated (file_hash + nar_hash + nar_size
    //    non-NULL; references may be NULL → treated as empty),
    //  - belong to an org cache with a private signing key,
    //  - lack a `cached_path_signature` row for that cache.
    //
    // A single query does the join so we get (org, store_path, nar_hash,
    // nar_size, references) tuples straight from SQL.
    let stmt = Statement::from_string(
        DatabaseBackend::Postgres,
        r#"
        SELECT DISTINCT
            oc.organization AS org_id,
            cp.store_path,
            cp.nar_hash,
            cp.nar_size,
            cp."references" AS refs
        FROM cached_path cp
        JOIN cache c ON c.private_key IS NOT NULL AND c.private_key <> ''
        JOIN organization_cache oc ON oc.cache = c.id
        WHERE cp.file_hash IS NOT NULL
          AND cp.nar_hash IS NOT NULL
          AND cp.nar_size IS NOT NULL
          AND NOT EXISTS (
              SELECT 1 FROM cached_path_signature cps
              WHERE cps.cached_path = cp.id AND cps.cache = c.id
          )
        ORDER BY cp.store_path
        LIMIT 10000
        "#,
    );

    let rows = match state.db.query_all(stmt).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "sign scanner: query failed");
            return Ok(());
        }
    };

    if rows.is_empty() {
        debug!("sign scanner: no unsigned cached paths found");
        return Ok(());
    }

    // Group by org_id, dedup by store_path within each group.
    let mut by_org: HashMap<Uuid, Vec<SignItem>> = HashMap::new();
    for row in rows {
        let Ok(org_id): Result<Uuid, _> = row.try_get("", "org_id") else {
            continue;
        };
        let Ok(store_path): Result<String, _> = row.try_get("", "store_path") else {
            continue;
        };
        let Ok(Some(nar_hash)): Result<Option<String>, _> = row.try_get("", "nar_hash") else {
            continue;
        };
        let Ok(Some(nar_size)): Result<Option<i64>, _> = row.try_get("", "nar_size") else {
            continue;
        };
        let references: Vec<String> = row
            .try_get::<Option<String>>("", "refs")
            .ok()
            .flatten()
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();

        by_org.entry(org_id).or_default().push(SignItem {
            store_path,
            nar_hash,
            nar_size: nar_size as u64,
            references,
        });
    }

    let now = chrono::Utc::now().naive_utc();
    let mut enqueued = 0usize;
    for (org_id, items) in by_org {
        for chunk in items.chunks(MAX_ITEMS_PER_JOB) {
            let job_id = format!("sign:{}:{}", org_id, Uuid::new_v4());
            let pending = PendingSignJob {
                peer_id: org_id,
                job: SignJob {
                    items: chunk.to_vec(),
                },
                queued_at: now,
            };
            scheduler.enqueue_sign_job(job_id, pending).await;
            enqueued += 1;
        }
    }

    if enqueued > 0 {
        info!(jobs = enqueued, "sign scanner: enqueued sign jobs");
    }
    Ok(())
}
