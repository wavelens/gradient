/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_db::DbContext;
use gradient_graph::{GcReport, GcRequest, Graph};
use gradient_storage::NarStore;
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use sea_orm::EntityTrait;
use tracing::{info, warn};

pub(crate) async fn cleanup_old_evaluations(db: &DbContext, graph: &Graph) -> Result<()> {
    let tasks = ETask::find()
        .all(&db.worker_db)
        .await
        .context("query tasks for evaluation GC")?;

    let mut removed = 0u64;
    for task in tasks {
        let keep = task.keep_evaluations as usize;
        if keep == 0 {
            continue;
        }

        let plan = match gradient_db::maintenance::gc::evaluation_gc_plan(db, task.id, keep).await {
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
            let report = match graph.gc(request).await {
                Ok(report) => report,
                Err(e) => {
                    warn!(error = %e, task_id = %task.id, "Evaluation GC failed for task");
                    break;
                }
            };

            delete_exec_outputs_of(&db.storage.nar_storage, &report).await;
            removed += report.deleted_evaluations.len() as u64;
            let deleted: Vec<MEvaluation> = chunk
                .iter()
                .filter(|e| report.deleted_evaluations.contains(&e.id))
                .cloned()
                .collect();
            if let Err(e) =
                gradient_db::maintenance::gc::after_evaluation_delete(db, &deleted).await
            {
                warn!(error = %e, task_id = %task.id, "Evaluation GC cleanup failed for task");
            }
        }
    }

    if removed > 0 {
        db.record(Swept {
            pass: Pass::Evaluations,
            removed,
        })
        .await;
    }
    Ok(())
}

async fn delete_exec_outputs_of(nar_storage: &NarStore, report: &GcReport) {
    for job in &report.deleted_exec_jobs {
        if let Err(e) = nar_storage.delete_exec_outputs(job.into_inner()).await {
            warn!(error = %e, %job, "Evaluation GC failed to delete exec outputs");
        }
    }
}

pub(crate) async fn collect_orphan_derivations(db: &DbContext, graph: &Graph) -> Result<usize> {
    let candidates = gradient_db::maintenance::gc::orphan_derivation_candidates(
        &db.worker_db,
        i64::from(db.config.gc.orphan_derivation_hours),
    )
    .await?;

    let mut deleted = 0usize;
    for chunk in candidates.chunks(gradient_db::IN_CHUNK_SIZE) {
        let report = match graph
            .gc(GcRequest::Derivations {
                candidates: chunk.to_vec(),
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
            if let Err(e) = db.storage.log_storage.delete(attempt).await {
                warn!(error = %e, %attempt, "GC: failed to remove orphan build log");
            }
        }
    }

    if deleted > 0 {
        info!(deleted, "Removed orphan derivations");
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn per_task_gc_deletes_the_outputs_of_its_exec_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path()).unwrap();
        let j1 = ExecJobId::now_v7();
        crate::test_support::store_exec_output(&nar, j1.into_inner(), staging.path()).await;

        let report = GcReport {
            deleted_exec_jobs: vec![j1],
            ..Default::default()
        };
        delete_exec_outputs_of(&nar, &report).await;

        assert!(nar.list_exec_output_jobs().await.unwrap().is_empty());
    }
}
