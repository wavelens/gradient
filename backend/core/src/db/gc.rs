/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, Statement,
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
                    | EvaluationStatus::Fetching
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
            // NAR files and GC roots are owned by `derivation_output` /
            // `cache_derivation` and are cleaned up by the derivation GC pass.
            let log_id = build.log_id.unwrap_or(build.id);
            if let Err(e) = state.log_storage.delete(log_id).await {
                warn!(error = %e, build_id = %log_id, "GC: failed to remove build log");
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

/// Derivation GC pass: deletes `derivation` rows that have no remaining `build` rows
/// pointing at them and whose grace period has expired. The grace lets rapid
/// re-evaluations reuse recent derivations without re-inserting.
///
/// For each orphan it:
///   1. Removes any `cache_derivation` rows (FK cascade also removes them, but doing it
///      explicitly lets us delete the NAR files first).
///   2. Removes the GC root for each `derivation_output`.
///   3. Deletes the derivation row — FK cascade cleans up outputs / dep edges / features /
///      signatures.
pub async fn gc_orphan_derivations(state: Arc<ServerState>, grace_hours: i64) -> Result<()> {
    let cutoff = Utc::now().naive_utc() - ChronoDuration::hours(grace_hours.max(0));

    // Find candidate derivations: no build rows, created before the cutoff.
    // Use raw SQL: SeaORM doesn't have a clean LEFT JOIN ... IS NULL builder.
    let rows = state
        .db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT d.id
               FROM derivation d
               LEFT JOIN build b ON b.derivation = d.id
               WHERE b.id IS NULL
                 AND d.created_at < $1"#,
            [sea_orm::Value::ChronoDateTime(Some(Box::new(cutoff)))],
        ))
        .await
        .context("Failed to query orphan derivations")?;

    let drv_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "id").ok())
        .collect();

    if drv_ids.is_empty() {
        return Ok(());
    }

    info!(count = drv_ids.len(), "Running orphan derivation GC");

    for drv_id in drv_ids {
        // 1. Cache presence rows + their NAR files.
        let cache_rows = ECacheDerivation::find()
            .filter(CCacheDerivation::Derivation.eq(drv_id))
            .all(&state.db)
            .await
            .context("Failed to query cache_derivation rows")?;

        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(drv_id))
            .all(&state.db)
            .await
            .context("Failed to query derivation outputs")?;

        for cache_row in cache_rows {
            // Once we drop the row, the (cache, derivation) pairing is gone.
            // FK cascade will also remove it on derivation delete, but doing it here
            // lets us first remove the NAR files.
            for o in &outputs {
                if let Err(e) = state.nar_storage.delete(&o.hash).await {
                    warn!(error = %e, hash = %o.hash, "GC: failed to remove NAR file");
                }
            }
            let _ = cache_row.into_active_model().delete(&state.db).await;
        }

        // 2. Remove GC roots for each output.
        for o in &outputs {
            remove_gcroot(&state, &o.hash, &o.package).await;
        }

        // 3. Delete the derivation row. FK cascade removes outputs / dep edges /
        //    features / signatures.
        if let Some(d) = EDerivation::find_by_id(drv_id)
            .one(&state.db)
            .await
            .context("GC: failed to load derivation")?
        {
            let a: ADerivation = d.into_active_model();
            if let Err(e) = a.delete(&state.db).await {
                warn!(error = %e, drv_id = %drv_id, "GC: failed to delete orphan derivation");
            }
        }
    }

    Ok(())
}
