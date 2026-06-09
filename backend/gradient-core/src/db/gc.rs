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
    QueryFilter, QueryOrder, Statement,
};
use tracing::{info, warn};
use uuid::Uuid;

use super::DbContext;
use crate::types::*;

/// Deletes evaluations for `project_id`, retaining the most recent `keep`
/// terminal evaluations (see [`evaluations_to_gc`]).
///
/// Handles DB deletion, build log removal, NAR cache files, and GC root symlinks.
/// Active evaluations are never deleted and never count toward `keep`.
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
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(eval.id))
            .all(&ctx.worker_db)
            .await
            .context("GC: failed to query builds")?;

        for build in &builds {
            // Remove the build log from all backing stores (local + S3).
            // NAR files and GC roots are owned by `derivation_output` /
            // `cache_derivation` and are cleaned up by the derivation GC pass.
            let log_id = build.log_id.unwrap_or(build.id);
            if let Err(e) = ctx.storage.log_storage.delete(log_id).await {
                warn!(error = %e, build_id = %log_id, "GC: failed to remove build log");
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
/// Active evaluations (Queued/Fetching/Evaluating*/Building/Waiting) are never
/// deleted and never consume a `keep` slot. Among terminal evaluations the
/// `keep` most recent `Completed`/`Failed` ("done") ones are retained; `Aborted`
/// evaluations are retained only to fill remaining slots when too few done
/// evaluations exist, and are deleted otherwise.
fn evaluations_to_gc(statuses: &[EvaluationStatus], keep: usize) -> Vec<usize> {
    if keep == 0 {
        return Vec::new();
    }

    let terminal: Vec<usize> = (0..statuses.len())
        .filter(|&i| !statuses[i].is_active())
        .collect();

    let mut retained = terminal.clone();
    retained.sort_by_key(|&i| matches!(statuses[i], EvaluationStatus::Aborted));
    let keep_set: std::collections::HashSet<usize> = retained.into_iter().take(keep).collect();

    terminal
        .into_iter()
        .filter(|i| !keep_set.contains(i))
        .collect()
}

/// Derivation GC pass: deletes `derivation` rows that have no remaining `build`
/// rows pointing at them and whose grace period has expired. The grace lets
/// rapid re-evaluations reuse recent derivations without re-inserting.
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

    let cutoff = crate::types::now() - ChronoDuration::hours(grace_hours.max(0));

    let rows = ctx
        .worker_db
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

    let drv_ids: Vec<DerivationId> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "id").ok().map(DerivationId::new))
        .collect();

    if drv_ids.is_empty() {
        return Ok(());
    }

    info!(count = drv_ids.len(), "Running orphan derivation GC");

    let db = &ctx.worker_db;
    let orphan_outputs = crate::db::fetch_in_chunks(&drv_ids, |chunk| async move {
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
        crate::db::fetch_in_chunks(&orphan_hash_vec, |chunk| async move {
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
        && let Err(e) = crate::db::for_each_chunk(&to_delete, |chunk| async move {
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
            .send(crate::types::BoardEvent::CacheChanged);
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
    fn keeps_last_done_when_newer_evaluation_is_active() {
        assert!(evaluations_to_gc(&[Building, Completed], 1).is_empty());
    }

    #[test]
    fn never_deletes_active_evaluations() {
        assert!(evaluations_to_gc(&[Queued, Building, Waiting, Fetching], 1).is_empty());
    }

    #[test]
    fn gcs_aborted_when_a_done_evaluation_exists() {
        assert_eq!(evaluations_to_gc(&[Aborted, Completed], 1), vec![0]);
    }

    #[test]
    fn keeps_aborted_when_no_done_evaluation_exists() {
        assert!(evaluations_to_gc(&[Aborted], 1).is_empty());
    }

    #[test]
    fn done_evaluations_take_priority_over_aborted() {
        assert_eq!(evaluations_to_gc(&[Completed, Aborted, Failed], 2), vec![1]);
    }

    #[test]
    fn deletes_done_evaluations_beyond_keep() {
        assert_eq!(
            evaluations_to_gc(&[Completed, Failed, Completed], 1),
            vec![1, 2]
        );
    }

    #[test]
    fn active_evaluations_do_not_consume_keep_slots() {
        assert_eq!(
            evaluations_to_gc(&[Building, Completed, Aborted, Completed], 1),
            vec![2, 3]
        );
    }

    #[test]
    fn keep_zero_deletes_nothing() {
        assert!(evaluations_to_gc(&[Completed, Aborted], 0).is_empty());
    }
}
