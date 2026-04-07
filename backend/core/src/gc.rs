/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::types::*;

pub async fn remove_gcroot(state: &Arc<ServerState>, hash: &str, package: &str) {
    let name = format!("{}-{}", hash, package);
    if let Err(e) = state.nix_store.remove_gcroot(name.clone()).await {
        warn!(error = %e, name = %name, "Failed to remove GC root");
    }
}

/// Deletes evaluations for `project_id` beyond the most recent `keep` entries.
///
/// Handles DB deletion, build log removal, NAR cache files, and GC root symlinks.
/// Never deletes evaluations that are still Queued/Evaluating/Building.
pub async fn gc_project_evaluations(
    state: Arc<ServerState>,
    project_id: Uuid,
    keep: usize,
) -> Result<()> {
    if keep == 0 {
        return Ok(());
    }

    // Newest first so [..keep] are the ones to retain.
    let all_evals = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project_id))
        .order_by_desc(CEvaluation::CreatedAt)
        .all(&state.db)
        .await
        .context("GC: failed to query evaluations")?;

    if all_evals.len() <= keep {
        return Ok(());
    }

    // Never GC evaluations that are still running.
    let to_delete: Vec<MEvaluation> = all_evals[keep..]
        .iter()
        .filter(|e| {
            !matches!(
                e.status,
                EvaluationStatus::Queued
                    | EvaluationStatus::EvaluatingFlake
                    | EvaluationStatus::EvaluatingDerivation
                    | EvaluationStatus::Building
                    | EvaluationStatus::Waiting
            )
        })
        .cloned()
        .collect();

    if to_delete.is_empty() {
        return Ok(());
    }

    info!(
        project_id = %project_id,
        deleting = to_delete.len(),
        "Running per-project evaluation GC"
    );

    // Break the linked list: NULL out `previous` on the oldest surviving evaluation so that
    // deleting the old evaluations does not cascade into the surviving chain.
    if let Some(oldest_surviving) = all_evals[..keep].last()
        && oldest_surviving.previous.is_some()
    {
        let mut a: AEvaluation = oldest_surviving.clone().into_active_model();
        a.previous = Set(None);
        a.update(&state.db)
            .await
            .context("GC: failed to unlink oldest surviving evaluation")?;
    }

    // NULL out previous/next on every evaluation being deleted so cascade between
    // deleted rows does not interfere with the deletion order.
    for eval in &to_delete {
        let mut a: AEvaluation = eval.clone().into_active_model();
        a.previous = Set(None);
        a.next = Set(None);
        a.update(&state.db)
            .await
            .context("GC: failed to NULL evaluation linked-list pointers")?;
    }

    for eval in &to_delete {
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(eval.id))
            .all(&state.db)
            .await
            .context("GC: failed to query builds")?;

        for build in &builds {
            // Remove the build log from all backing stores (local + S3).
            let log_id = build.log_id.unwrap_or(build.id);
            if let Err(e) = state.log_storage.delete(log_id).await {
                warn!(error = %e, build_id = %log_id, "GC: failed to remove build log");
            }

            // Remove cached NAR files and GC root symlinks for each build output.
            let outputs = EBuildOutput::find()
                .filter(CBuildOutput::Build.eq(build.id))
                .filter(CBuildOutput::IsCached.eq(true))
                .all(&state.db)
                .await
                .context("GC: failed to query build outputs")?;

            for output in &outputs {
                remove_gcroot(&state, &output.hash, &output.package).await;
                if let Err(e) = state.nar_storage.delete(&output.hash).await {
                    warn!(error = %e, hash = %output.hash, "GC: failed to remove NAR");
                }
            }
        }

        // Collect commit ID before deletion (not cascaded).
        let commit_id = eval.commit;

        let a: AEvaluation = eval.clone().into_active_model();
        a.delete(&state.db)
            .await
            .context("GC: failed to delete evaluation")?;

        // Clean up the commit record if no other evaluation references it.
        let still_referenced = EEvaluation::find()
            .filter(CEvaluation::Commit.eq(commit_id))
            .one(&state.db)
            .await
            .context("GC: failed to check commit references")?;

        if still_referenced.is_none()
            && let Some(c) = ECommit::find_by_id(commit_id)
                .one(&state.db)
                .await
                .context("GC: failed to query commit")?
        {
            let ac: ACommit = c.into_active_model();
            if let Err(e) = ac.delete(&state.db).await {
                warn!(error = %e, commit_id = %commit_id, "GC: failed to delete orphaned commit");
            }
        }
    }

    info!(project_id = %project_id, deleted = to_delete.len(), "Per-project evaluation GC done");
    Ok(())
}
