/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_graph::GcRequest;
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tracing::{info, warn};

pub(crate) async fn cleanup_old_evaluations(state: Arc<ServerState>) -> Result<()> {
    let tasks = ETask::find()
        .all(&state.worker_db)
        .await
        .context("Failed to query tasks for evaluation GC")?;

    let ctx = state.db();
    let mut removed = 0u64;
    for task in tasks {
        let keep = task.keep_evaluations as usize;
        if keep == 0 {
            continue;
        }

        let plan = match gradient_db::maintenance::gc::evaluation_gc_plan(&ctx, task.id, keep).await
        {
            Ok(plan) if plan.is_empty() => continue,
            Ok(plan) => plan,
            Err(e) => {
                warn!(error = %e, task_id = %task.id, "Evaluation GC selection failed for task");
                continue;
            }
        };

        for chunk in plan.chunks(gradient_db::IN_CHUNK_SIZE) {
            let request = GcRequest::Evaluations {
                ids: chunk.iter().map(|e| e.id).collect(),
            };
            let report = match state.graph.gc(request).await {
                Ok(report) => report,
                Err(e) => {
                    warn!(error = %e, task_id = %task.id, "Evaluation GC failed for task");
                    break;
                }
            };

            removed += report.deleted_evaluations.len() as u64;
            let deleted: Vec<MEvaluation> = chunk
                .iter()
                .filter(|e| report.deleted_evaluations.contains(&e.id))
                .cloned()
                .collect();
            if let Err(e) =
                gradient_db::maintenance::gc::after_evaluation_delete(&ctx, &deleted).await
            {
                warn!(error = %e, task_id = %task.id, "Evaluation GC cleanup failed for task");
            }
        }
    }

    if removed > 0 {
        state
            .record(Swept {
                pass: Pass::Evaluations,
                removed,
            })
            .await;
    }
    Ok(())
}

pub(crate) async fn collect_orphan_derivations(state: &Arc<ServerState>) -> Result<usize> {
    let (candidates, scanned_at) = gradient_db::maintenance::gc::orphan_derivation_candidates(
        &state.worker_db,
        state.config.gc.orphan_derivation_hours,
    )
    .await?;

    let mut deleted = 0usize;
    for chunk in candidates.chunks(gradient_db::IN_CHUNK_SIZE) {
        let report = match state
            .graph
            .gc(GcRequest::Derivations {
                candidates: chunk.to_vec(),
                scanned_at,
            })
            .await
        {
            Ok(report) => report,
            Err(e) => {
                warn!(error = %e, "GC: orphan derivation delete chunk failed; skipping");
                continue;
            }
        };

        deleted += report.deleted_derivations.len();
        for attempt in report.attempt_logs {
            if let Err(e) = state.log_storage.delete(attempt).await {
                warn!(error = %e, %attempt, "GC: failed to remove orphan build log");
            }
        }
    }

    if deleted > 0 {
        info!(deleted, "Removed orphan derivations");
    }
    Ok(deleted)
}
