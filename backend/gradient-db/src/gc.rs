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

    let evals: Vec<(EvaluationStatus, chrono::NaiveDateTime)> =
        all_evals.iter().map(|e| (e.status, e.updated_at)).collect();
    let delete_indices = evaluations_to_gc(
        &evals,
        keep,
        ctx.config.storage.gc_wedged_eval_hours,
        gradient_types::now(),
    );
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
        // The eval's `build_job` rows cascade away, but its `build_attempt` rows
        // (and their logs) are set-null'd onto the surviving `derivation_build`
        // anchor - their true, build-once owner - and reclaimed only when the
        // derivation GC deletes that anchor. NAR files and GC roots are likewise
        // owned by `derivation_output` / `cache_derivation` and cleaned up there.

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
/// Returns nothing while any evaluation is genuinely active (Queued/Fetching/
/// Evaluating*/Building/Waiting): an in-flight run may reuse NARs from older
/// evaluations before it records its own build rows, so GC waits until the
/// project is quiescent. An "active" evaluation untouched for more than
/// `wedged_hours` is presumed wedged and stops blocking - otherwise one stuck
/// run silently turns a scheduler bug into unbounded storage growth
/// (`wedged_hours = 0` restores the unconditional block). Wedged evaluations
/// are never deleted themselves; the `keep` most recent terminal evaluations
/// are retained regardless of outcome - `Failed` and `Aborted` runs can still
/// hold successfully-built NARs, so they are not sacrificed ahead of newer
/// `Completed` ones.
fn evaluations_to_gc(
    evals: &[(EvaluationStatus, chrono::NaiveDateTime)],
    keep: usize,
    wedged_hours: i64,
    now: chrono::NaiveDateTime,
) -> Vec<usize> {
    if keep == 0 {
        return Vec::new();
    }

    let blocking_active = evals.iter().any(|(status, updated_at)| {
        status.is_active()
            && (wedged_hours <= 0 || now - *updated_at < ChronoDuration::hours(wedged_hours))
    });
    if blocking_active {
        return Vec::new();
    }

    let mut kept = 0usize;
    let mut delete = Vec::new();
    for (i, (status, _)) in evals.iter().enumerate() {
        if status.is_active() {
            continue;
        }
        if kept < keep {
            kept += 1;
        } else {
            delete.push(i);
        }
    }

    delete
}

/// Derivation GC pass (mark-and-sweep): deletes global `derivation` rows that lie
/// *outside the build-dependency closure of every live root* - an `entry_point` or
/// a derivation a retained eval's `build_job` references - and whose grace period
/// has expired. The grace lets rapid re-evaluations reuse recent derivations.
///
/// Reachability matters because `build_job` rows are pruned with old evals while
/// `derivation_dependency` edges and anchors persist: a derivation still needed as
/// a build input of a retained closure (its own evals long gone) has no `build_job`
/// yet must be kept. A naive "no `build_job`" test reclaimed those, deleting build
/// inputs of live anchors and stranding dependents on `InputsUnavailable`.
///
/// The rows are deleted first, re-checking the orphan predicate inside the
/// statement: because derivations are global and content-addressed, a
/// concurrent evaluation can re-attach a `build_job` to a past-grace orphan at
/// any moment, so a single SELECT-then-delete would race the FK. `RETURNING`
/// then reports exactly which rows went, and NAR reclaim is keyed strictly to
/// those - a hash still referenced by any surviving `derivation_output` (FOD
/// source tarballs shared via `fetchurl`) keeps its NAR and `cached_path` row.
/// FK cascade cleans up `derivation_output`, `derivation_build`, dep/closure
/// edges, features, metrics, and `cache_derivation`; `cached_path_signature`
/// cascades from `cached_path`.
pub async fn gc_orphan_derivations(ctx: &DbContext, grace_hours: i64) -> Result<()> {
    use std::collections::HashSet;

    let cutoff = gradient_types::now() - ChronoDuration::hours(grace_hours.max(0));
    let db = &ctx.worker_db;

    // The keep-set is the shared build-graph walk (`graph_sql`), reused verbatim
    // by the candidate scan and the delete re-check so they can never diverge.
    let reachable = crate::graph_sql::reachable_derivations_cte();
    let select_sql = format!(
        "{reachable}
         SELECT d.id, d.hash FROM derivation d
         WHERE d.created_at < $1
           AND NOT EXISTS (SELECT 1 FROM reachable rc WHERE rc.derivation = d.id)"
    );
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &select_sql,
            [sea_orm::Value::ChronoDateTime(Some(Box::new(cutoff)))],
        ))
        .await
        .context("Failed to query orphan derivations")?;

    // Capture each candidate's own `.drv` hash before deletion so the reclaim
    // set can drop the `.drv` NAR + cached_path, not just the outputs.
    let candidate_drv_hash: std::collections::HashMap<DerivationId, String> = rows
        .iter()
        .filter_map(|r| {
            let id = r.try_get::<Uuid>("", "id").ok().map(DerivationId::new)?;
            let hash = r.try_get::<String>("", "hash").ok()?;
            Some((id, hash))
        })
        .collect();
    let candidate_ids: Vec<DerivationId> = candidate_drv_hash.keys().copied().collect();

    if candidate_ids.is_empty() {
        return Ok(());
    }

    info!(count = candidate_ids.len(), "Running orphan derivation GC");

    // Pin candidate output hashes before deletion: a deleted derivation's
    // `derivation_output` rows cascade away, so the NAR reclaim set is derived
    // from this snapshot intersected with what actually gets deleted.
    let candidate_outputs = crate::fetch_in_chunks(&candidate_ids, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("GC: failed to query orphan derivation outputs")?;

    // Snapshot each candidate derivation's build-attempt logs before the delete
    // cascades the attempt rows away: their log files live in `log_storage`
    // (keyed by attempt id), outside the DB, so they must be reclaimed by hand
    // like the NARs. `pass_logs` in the deep GC is the backstop for any missed.
    let candidate_anchors = crate::fetch_in_chunks(&candidate_ids, |chunk| async move {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("GC: failed to query orphan derivation anchors")?;
    let anchor_derivation: std::collections::HashMap<DerivationBuildId, DerivationId> =
        candidate_anchors
            .iter()
            .map(|a| (a.id, a.derivation))
            .collect();
    let anchor_ids: Vec<DerivationBuildId> = anchor_derivation.keys().copied().collect();

    let candidate_attempts = crate::fetch_in_chunks(&anchor_ids, |chunk| async move {
        EBuildAttempt::find()
            .filter(CBuildAttempt::DerivationBuild.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("GC: failed to query orphan derivation build attempts")?;
    let attempt_snapshot: Vec<(DerivationId, BuildAttemptId)> = candidate_attempts
        .iter()
        .filter_map(|a| {
            anchor_derivation
                .get(&a.derivation_build)
                .map(|d| (*d, a.id))
        })
        .collect();

    let delete_sql = format!(
        "{reachable}
         DELETE FROM derivation d
         WHERE d.id = ANY($1)
           AND NOT EXISTS (SELECT 1 FROM reachable rc WHERE rc.derivation = d.id)
         RETURNING d.id"
    );
    let mut deleted: HashSet<DerivationId> = HashSet::new();
    for chunk in candidate_ids.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Vec<Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        match db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &delete_sql,
                [ids.into()],
            ))
            .await
        {
            Ok(returned) => deleted.extend(
                returned
                    .iter()
                    .filter_map(|r| r.try_get::<Uuid>("", "id").ok().map(DerivationId::new)),
            ),
            Err(e) => warn!(error = %e, "GC: orphan derivation delete chunk failed; skipping"),
        }
    }

    if deleted.is_empty() {
        return Ok(());
    }

    // Reclaim the log files of every attempt whose derivation was just deleted;
    // their `build_attempt`/`build_log_chunk` rows already cascaded away.
    for attempt_id in attempt_logs_to_reclaim(&attempt_snapshot, &deleted) {
        if let Err(e) = ctx.storage.log_storage.delete(attempt_id).await {
            warn!(error = %e, %attempt_id, "GC: failed to remove orphan build log");
        }
    }

    let deleted_hashes: HashSet<String> = candidate_outputs
        .iter()
        .filter(|o| deleted.contains(&o.derivation))
        .map(|o| o.hash.clone())
        .collect();

    // Post-delete, any surviving `derivation_output` for a deleted hash belongs
    // to a still-live derivation that shares it, so its NAR must be kept.
    let still_referenced: HashSet<String> = if deleted_hashes.is_empty() {
        HashSet::new()
    } else {
        let hash_vec: Vec<String> = deleted_hashes.iter().cloned().collect();
        crate::fetch_in_chunks(&hash_vec, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("GC: failed to query surviving references for deleted output hashes")?
        .into_iter()
        .map(|o| o.hash)
        .collect()
    };

    // A derivation's own `.drv` NAR + cached_path are reclaimed too: they were
    // never tied to a `derivation_output`, so the old output-only reclaim leaked
    // them on every orphan sweep. Keep a `.drv` hash only if a concurrent eval
    // re-created the derivation (a surviving `derivation` row shares it).
    let deleted_drv_hashes: HashSet<String> = deleted
        .iter()
        .filter_map(|id| candidate_drv_hash.get(id).cloned())
        .collect();
    let surviving_drv_hashes: HashSet<String> = if deleted_drv_hashes.is_empty() {
        HashSet::new()
    } else {
        let hash_vec: Vec<String> = deleted_drv_hashes.iter().cloned().collect();
        crate::fetch_in_chunks(&hash_vec, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("GC: failed to query surviving derivations for deleted drv hashes")?
        .into_iter()
        .map(|d| d.hash)
        .collect()
    };

    let to_delete = reclaimable_after_delete(
        deleted_hashes,
        &still_referenced,
        deleted_drv_hashes,
        &surviving_drv_hashes,
    );

    for hash in &to_delete {
        if let Err(e) = ctx.storage.nar_storage.delete(hash).await {
            warn!(error = %e, %hash, "GC: failed to remove NAR file");
        }
    }

    // Delete the rows and clear the gate flags they backed in one transaction:
    // a `drv_closure_cached`/`closure_complete` anchor must never trust a
    // `cached_path` this pass just removed, not even until the next reconcile.
    if !to_delete.is_empty()
        && let Err(e) = crate::for_each_chunk(&to_delete, |chunk| async move {
            use sea_orm::TransactionTrait;
            let txn = db.inner().begin().await?;
            ECachedPath::delete_many()
                .filter(CCachedPath::Hash.is_in(chunk.clone()))
                .exec(&txn)
                .await?;
            crate::cache_storage::clear_gate_flags_for_hashes(&txn, &chunk).await?;
            txn.commit().await
        })
        .await
    {
        warn!(error = %e, "GC: failed to delete cached_path rows for orphan hashes");
    }

    let _ = ctx
        .board_events
        .send(gradient_types::BoardEvent::CacheChanged);

    info!(
        deleted = deleted.len(),
        reclaimed_nars = to_delete.len(),
        "Orphan derivation GC done"
    );
    Ok(())
}

/// Of the output hashes belonging to just-deleted derivations, the ones whose
/// NAR and `cached_path` can be reclaimed: those no surviving
/// `derivation_output` still references.
fn reclaimable_hashes(
    deleted_hashes: std::collections::HashSet<String>,
    still_referenced: &std::collections::HashSet<String>,
) -> Vec<String> {
    deleted_hashes
        .into_iter()
        .filter(|h| !still_referenced.contains(h))
        .collect()
}

/// Every NAR/`cached_path` hash reclaimable after an orphan-derivation sweep:
/// the deleted derivations' output hashes (minus any a surviving
/// `derivation_output` still shares) plus their own `.drv` hashes (minus any a
/// concurrent eval re-created as a surviving `derivation`). The two guards
/// differ - outputs against surviving outputs, `.drv`s against surviving
/// derivations - because a derivation's own `.drv` has no `derivation_output`.
fn reclaimable_after_delete(
    deleted_output_hashes: std::collections::HashSet<String>,
    surviving_output_hashes: &std::collections::HashSet<String>,
    deleted_drv_hashes: std::collections::HashSet<String>,
    surviving_drv_hashes: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut out = reclaimable_hashes(deleted_output_hashes, surviving_output_hashes);
    out.extend(reclaimable_hashes(deleted_drv_hashes, surviving_drv_hashes));
    out
}

/// From a pre-delete `(derivation, attempt)` snapshot, the attempt ids whose
/// derivation was actually reclaimed - their `log_storage` files can now be
/// deleted. Attempts of derivations that survived the delete re-check (a
/// concurrent eval re-attached a `build_job`) keep their logs.
fn attempt_logs_to_reclaim(
    snapshot: &[(DerivationId, BuildAttemptId)],
    deleted: &std::collections::HashSet<DerivationId>,
) -> Vec<BuildAttemptId> {
    snapshot
        .iter()
        .filter(|(derivation, _)| deleted.contains(derivation))
        .map(|(_, attempt)| *attempt)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use EvaluationStatus::*;
    use std::collections::HashSet;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reclaims_only_hashes_no_survivor_references() {
        // `shared` is still referenced by a surviving derivation_output (e.g. a
        // fetchurl source tarball), so only `solo` is reclaimable.
        let mut got = reclaimable_hashes(set(&["solo", "shared"]), &set(&["shared"]));
        got.sort();
        assert_eq!(got, vec!["solo".to_string()]);
    }

    #[test]
    fn reclaims_drv_hash_unless_a_surviving_derivation_recreated_it() {
        // A deleted derivation's own `.drv` hash is reclaimed alongside its
        // outputs, but `d2` was re-created by a concurrent eval (surviving
        // `derivation` row shares the hash) so it must be kept.
        let mut got =
            reclaimable_after_delete(set(&["o1"]), &set(&[]), set(&["d1", "d2"]), &set(&["d2"]));
        got.sort();
        assert_eq!(got, vec!["d1".to_string(), "o1".to_string()]);
    }

    #[test]
    fn reclaims_nothing_when_all_hashes_survive() {
        assert!(reclaimable_hashes(set(&["a", "b"]), &set(&["a", "b"])).is_empty());
    }

    #[test]
    fn reclaims_attempt_logs_only_for_deleted_derivations() {
        // `d_kept` survived the delete re-check (a concurrent eval re-attached a
        // build_job), so its attempt's log is retained; `d_gone`'s is reclaimed.
        let d_gone = DerivationId::now_v7();
        let d_kept = DerivationId::now_v7();
        let a_gone = BuildAttemptId::now_v7();
        let a_kept = BuildAttemptId::now_v7();
        let snapshot = vec![(d_gone, a_gone), (d_kept, a_kept)];
        let deleted: HashSet<DerivationId> = [d_gone].into_iter().collect();
        assert_eq!(attempt_logs_to_reclaim(&snapshot, &deleted), vec![a_gone]);
    }

    #[test]
    fn reclaims_all_when_no_survivors() {
        let mut got = reclaimable_hashes(set(&["a", "b"]), &set(&[]));
        got.sort();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
    }

    const WEDGED_HOURS: i64 = 24;

    fn at(
        statuses: &[EvaluationStatus],
        age_hours: i64,
    ) -> Vec<(EvaluationStatus, chrono::NaiveDateTime)> {
        let updated = gradient_types::now() - ChronoDuration::hours(age_hours);
        statuses.iter().map(|s| (*s, updated)).collect()
    }

    fn gc(statuses: &[EvaluationStatus], keep: usize) -> Vec<usize> {
        evaluations_to_gc(&at(statuses, 1), keep, WEDGED_HOURS, gradient_types::now())
    }

    #[test]
    fn skips_gc_while_an_evaluation_is_active() {
        // An in-flight run may still reuse older NARs, so nothing is deleted -
        // even completed evaluations far beyond `keep` are retained this pass.
        assert!(gc(&[Building, Completed], 1).is_empty());
        assert!(gc(&[Queued, Building, Waiting, Fetching], 1).is_empty());
        assert!(gc(&[Building, Completed, Aborted, Completed], 1).is_empty());
    }

    #[test]
    fn wedged_active_evaluation_stops_blocking_but_is_never_deleted() {
        // An eval "active" for longer than the wedged threshold no longer
        // freezes the project's GC; terminal evals beyond keep are reclaimed,
        // the wedged eval itself is skipped.
        let evals = at(&[Building, Completed, Failed, Completed], 48);
        assert_eq!(
            evaluations_to_gc(&evals, 1, WEDGED_HOURS, gradient_types::now()),
            vec![2, 3]
        );
        // wedged_hours = 0 restores the unconditional block.
        assert!(evaluations_to_gc(&evals, 1, 0, gradient_types::now()).is_empty());
    }

    #[test]
    fn retains_keep_most_recent_terminal_regardless_of_outcome() {
        // A newer Aborted/Failed run is kept ahead of an older Completed one;
        // its successfully-built NARs are not sacrificed.
        assert_eq!(gc(&[Aborted, Completed], 1), vec![1]);
        assert_eq!(gc(&[Completed, Aborted, Failed], 2), vec![2]);
    }

    #[test]
    fn keeps_single_terminal_within_keep() {
        assert!(gc(&[Aborted], 1).is_empty());
        assert!(gc(&[Failed], 1).is_empty());
    }

    #[test]
    fn deletes_terminal_evaluations_beyond_keep() {
        assert_eq!(gc(&[Completed, Failed, Completed], 1), vec![1, 2]);
    }

    #[test]
    fn keep_zero_deletes_nothing() {
        assert!(gc(&[Completed, Aborted], 0).is_empty());
    }
}
