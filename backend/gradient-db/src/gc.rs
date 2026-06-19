/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::Duration as ChronoDuration;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, QuerySelect, Statement,
};
use tracing::{info, warn};
use uuid::Uuid;

use super::DbContext;
use gradient_types::*;

/// Deletes evaluations for `project_id`, retaining the most recent `keep`
/// terminal evaluations (see [`evaluations_to_gc`]).
///
/// Handles DB deletion, build log removal, NAR cache files, and GC root symlinks.
/// Skipped entirely while the project has any active evaluation, so an in-flight
/// run never loses NARs it is about to reuse.
pub async fn gc_project_evaluations(
    ctx: &DbContext,
    project_id: ProjectId,
    keep: usize,
) -> Result<()> {
    if keep == 0 {
        return Ok(());
    }

    // Newest first; deletion selection counts only terminal evaluations.
    let all_evals = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project_id))
        .order_by_desc(CEvaluation::CreatedAt)
        .all(&ctx.worker_db)
        .await
        .context("GC: failed to query evaluations")?;

    let statuses: Vec<EvaluationStatus> = all_evals.iter().map(|e| e.status).collect();
    let delete_indices = evaluations_to_gc(&statuses, keep);
    if delete_indices.is_empty() {
        return Ok(());
    }

    let to_delete: Vec<MEvaluation> = delete_indices
        .iter()
        .map(|&i| all_evals[i].clone())
        .collect();
    let deleted_ids: std::collections::HashSet<EvaluationId> =
        to_delete.iter().map(|e| e.id).collect();

    info!(
        project_id = %project_id,
        deleting = to_delete.len(),
        "Running per-project evaluation GC"
    );

    // Break the linked list so deletions never violate previous/next FKs:
    // NULL the deleted rows' own pointers and any surviving pointer into them.
    for eval in &all_evals {
        let drop_prev = eval.previous.is_some_and(|p| deleted_ids.contains(&p));
        let drop_next = eval.next.is_some_and(|n| deleted_ids.contains(&n));
        if !deleted_ids.contains(&eval.id) && !drop_prev && !drop_next {
            continue;
        }

        let mut a: AEvaluation = eval.clone().into_active_model();
        if deleted_ids.contains(&eval.id) || drop_prev {
            a.previous = Set(None);
        }
        if deleted_ids.contains(&eval.id) || drop_next {
            a.next = Set(None);
        }
        a.update(&ctx.worker_db)
            .await
            .context("GC: failed to NULL evaluation linked-list pointers")?;
    }

    for eval in &to_delete {
        // Remove the log of every attempt attributed to this eval's build_jobs
        // (logs are keyed by attempt id). DB rows cascade with the eval; NAR
        // files and GC roots are owned by `derivation_output` / `cache_derivation`
        // and are cleaned up by the derivation GC pass.
        let job_ids: Vec<gradient_types::ids::BuildJobId> = EBuildJob::find()
            .select_only()
            .column(CBuildJob::Id)
            .filter(CBuildJob::Evaluation.eq(eval.id))
            .into_tuple::<gradient_types::ids::BuildJobId>()
            .all(&ctx.worker_db)
            .await
            .context("GC: failed to query build_jobs")?;

        let attempts = crate::fetch_in_chunks(&job_ids, |chunk| async move {
            gradient_entity::build_attempt::Entity::find()
                .filter(gradient_entity::build_attempt::Column::BuildJob.is_in(chunk))
                .all(&ctx.worker_db)
                .await
        })
        .await
        .context("GC: failed to query build attempts")?;

        for att in &attempts {
            if let Err(e) = ctx.storage.log_storage.delete(att.id).await {
                warn!(error = %e, attempt_id = %att.id, "GC: failed to remove build log");
            }
        }

        // Collect commit ID before deletion (not cascaded).
        let commit_id = eval.commit;

        let a: AEvaluation = eval.clone().into_active_model();
        a.delete(&ctx.worker_db)
            .await
            .context("GC: failed to delete evaluation")?;

        // Clean up the commit record if no other evaluation references it.
        let still_referenced = EEvaluation::find()
            .filter(CEvaluation::Commit.eq(commit_id))
            .one(&ctx.worker_db)
            .await
            .context("GC: failed to check commit references")?;

        if still_referenced.is_none()
            && let Some(c) = ECommit::find_by_id(commit_id)
                .one(&ctx.worker_db)
                .await
                .context("GC: failed to query commit")?
        {
            let ac: ACommit = c.into_active_model();
            if let Err(e) = ac.delete(&ctx.worker_db).await {
                warn!(error = %e, commit_id = %commit_id, "GC: failed to delete orphaned commit");
            }
        }
    }

    info!(project_id = %project_id, deleted = to_delete.len(), "Per-project evaluation GC done");
    Ok(())
}

/// Selects, by index into a newest-first evaluation list, which evaluations the
/// per-project GC should delete for a given `keep` count.
///
/// Returns nothing while any evaluation is active (Queued/Fetching/Evaluating*/
/// Building/Waiting): an in-flight run may reuse NARs from older evaluations
/// before it records its own build rows, so GC waits until the project is
/// quiescent. Otherwise the `keep` most recent terminal evaluations are retained
/// regardless of outcome - `Failed` and `Aborted` runs can still hold
/// successfully-built NARs, so they are not sacrificed ahead of newer
/// `Completed` ones.
fn evaluations_to_gc(statuses: &[EvaluationStatus], keep: usize) -> Vec<usize> {
    if keep == 0 || statuses.iter().any(|s| s.is_active()) {
        return Vec::new();
    }

    (keep..statuses.len()).collect()
}

/// Derivation GC pass: deletes `derivation` rows no surviving evaluation needs
/// (no `build_job` references them) and whose grace period has expired. The
/// grace lets rapid re-evaluations reuse recent derivations without re-inserting.
///
/// NAR deletion is keyed by the orphan-only output hashes - a hash referenced
/// by any *non-orphan* `derivation_output` (typical for FOD source tarballs
/// that many drvs share via `fetchurl`) keeps both its NAR and its
/// `cached_path` row. For hashes referenced only by orphans, both the NAR
/// file and the `cached_path` row are removed; `cached_path_signature`
/// cascades from `cached_path`. The derivation rows are deleted last; FK
/// cascade cleans up `cache_derivation`, `derivation_output`, dep edges,
/// and feature edges.
pub async fn gc_orphan_derivations(ctx: &DbContext, grace_hours: i64) -> Result<()> {
    use std::collections::HashSet;

    let cutoff = gradient_types::now() - ChronoDuration::hours(grace_hours.max(0));

    let rows = ctx
        .worker_db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT d.id
               FROM derivation d
               LEFT JOIN build_job bj ON bj.derivation = d.id
               WHERE bj.id IS NULL
                 AND d.created_at < $1"#,
            [sea_orm::Value::ChronoDateTime(Some(Box::new(cutoff)))],
        ))
        .await
        .context("Failed to query orphan derivations")?;

    let drv_ids: Vec<DerivationId> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "id").ok().map(DerivationId::new))
        .collect();

    if drv_ids.is_empty() {
        return Ok(());
    }

    info!(count = drv_ids.len(), "Running orphan derivation GC");

    let db = &ctx.worker_db;
    let orphan_outputs = crate::fetch_in_chunks(&drv_ids, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("GC: failed to query orphan derivation outputs")?;

    let orphan_hashes: HashSet<String> = orphan_outputs.iter().map(|o| o.hash.clone()).collect();

    let still_referenced: HashSet<String> = if orphan_hashes.is_empty() {
        HashSet::new()
    } else {
        let drv_id_set: HashSet<DerivationId> = drv_ids.iter().copied().collect();
        let orphan_hash_vec: Vec<String> = orphan_hashes.iter().cloned().collect();
        crate::fetch_in_chunks(&orphan_hash_vec, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("GC: failed to query non-orphan references for orphan output hashes")?
        .into_iter()
        .filter(|o| !drv_id_set.contains(&o.derivation))
        .map(|o| o.hash)
        .collect()
    };

    let to_delete: Vec<String> = orphan_hashes
        .into_iter()
        .filter(|h| !still_referenced.contains(h))
        .collect();

    for hash in &to_delete {
        if let Err(e) = ctx.storage.nar_storage.delete(hash).await {
            warn!(error = %e, %hash, "GC: failed to remove NAR file");
        }
    }

    if !to_delete.is_empty()
        && let Err(e) = crate::for_each_chunk(&to_delete, |chunk| async move {
            ECachedPath::delete_many()
                .filter(CCachedPath::Hash.is_in(chunk))
                .exec(db)
                .await
        })
        .await
    {
        warn!(error = %e, "GC: failed to delete cached_path rows for orphan hashes");
    }

    if !to_delete.is_empty() {
        let _ = ctx
            .board_events
            .send(gradient_types::BoardEvent::CacheChanged);
    }

    for drv_id in drv_ids {
        if let Some(d) = EDerivation::find_by_id(drv_id)
            .one(&ctx.worker_db)
            .await
            .context("GC: failed to load derivation")?
        {
            let a: ADerivation = d.into_active_model();
            if let Err(e) = a.delete(&ctx.worker_db).await {
                warn!(error = %e, drv_id = %drv_id, "GC: failed to delete orphan derivation");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use EvaluationStatus::*;

    #[test]
    fn skips_gc_while_an_evaluation_is_active() {
        // An in-flight run may still reuse older NARs, so nothing is deleted -
        // even completed evaluations far beyond `keep` are retained this pass.
        assert!(evaluations_to_gc(&[Building, Completed], 1).is_empty());
        assert!(evaluations_to_gc(&[Queued, Building, Waiting, Fetching], 1).is_empty());
        assert!(evaluations_to_gc(&[Building, Completed, Aborted, Completed], 1).is_empty());
    }

    #[test]
    fn retains_keep_most_recent_terminal_regardless_of_outcome() {
        // A newer Aborted/Failed run is kept ahead of an older Completed one;
        // its successfully-built NARs are not sacrificed.
        assert_eq!(evaluations_to_gc(&[Aborted, Completed], 1), vec![1]);
        assert_eq!(evaluations_to_gc(&[Completed, Aborted, Failed], 2), vec![2]);
    }

    #[test]
    fn keeps_single_terminal_within_keep() {
        assert!(evaluations_to_gc(&[Aborted], 1).is_empty());
        assert!(evaluations_to_gc(&[Failed], 1).is_empty());
    }

    #[test]
    fn deletes_terminal_evaluations_beyond_keep() {
        assert_eq!(
            evaluations_to_gc(&[Completed, Failed, Completed], 1),
            vec![1, 2]
        );
    }

    #[test]
    fn keep_zero_deletes_nothing() {
        assert!(evaluations_to_gc(&[Completed, Aborted], 0).is_empty());
    }
}
