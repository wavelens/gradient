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
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder,
};
use tracing::{info, warn};
use uuid::Uuid;

use super::DbContext;
use gradient_types::*;

crate::sql! {
    ANCHORS_LOSING_AN_EVALUATION = "SELECT derivation FROM build_job WHERE evaluation = ANY($1::uuid[]) \
             UNION SELECT derivation FROM entry_point WHERE evaluation = ANY($1::uuid[]) \
             UNION SELECT e.dependency AS derivation FROM derivation_dependency e \
             JOIN build_job bj ON bj.derivation = e.derivation \
             WHERE bj.evaluation = ANY($1::uuid[])",
        params = [BuildIds(64)],
        tier = Sweep;

    ORPHAN_DEPENDENTS = "SELECT e.derivation, e.dependency FROM derivation_dependency e \
                 WHERE e.dependency = ANY($1)",
        params = [DerivationIds(64)],
        tier = Sweep;

    UNWALK_ORPHAN_SURVIVORS = "UPDATE derivation SET walked = false WHERE id = ANY($1)",
        params = [DerivationIds(64)],
        tier = Sweep;
}

/// The keep-set is the shared build-graph walk (`graph_sql`), reused verbatim by
/// the candidate scan and the delete re-check so they can never diverge.
fn gc_orphan_candidates_sql() -> String {
    format!(
        "{reachable}
         SELECT d.id FROM derivation d
         WHERE d.created_at < $1
           AND NOT EXISTS (SELECT 1 FROM reachable rc WHERE rc.derivation = d.id)",
        reachable = crate::graph_sql::reachable_derivations_cte(),
    )
}

crate::sql_fn! {
    GC_ORPHAN_CANDIDATES = gc_orphan_candidates_sql,
        params = [Now],
        tier = Walk,
        flags = [Walk];
}

fn gc_orphan_delete_sql() -> String {
    format!(
        "{reachable}
         DELETE FROM derivation d
         WHERE d.id = ANY($1)
           AND NOT EXISTS (SELECT 1 FROM reachable rc WHERE rc.derivation = d.id)
         RETURNING d.id",
        reachable = crate::graph_sql::reachable_derivations_cte(),
    )
}

crate::sql_fn! {
    GC_ORPHAN_DELETE = gc_orphan_delete_sql,
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

/// The bound is one parameter, and `make_interval` takes an `integer`, so the
/// cast is in the text rather than in whatever the caller happened to bind.
fn stale_cached_paths_sql() -> String {
    format!(
        "{live}
         SELECT cp.hash FROM cached_path cp
         WHERE NOT EXISTS (SELECT 1 FROM live l WHERE l.hash = cp.hash)
           AND coalesce((SELECT max(s.last_fetched_at) FROM cached_path_signature s
                         WHERE s.cached_path = cp.id), cp.created_at)
               < (now() AT TIME ZONE 'UTC') - make_interval(hours => $1::int)",
        live = crate::graph_sql::live_cached_paths_cte(),
    )
}

crate::sql_fn! {
    STALE_CACHED_PATHS = stale_cached_paths_sql,
        params = [Int(336)],
        tier = Walk,
        flags = [Walk];
}

/// Paths no retained evaluation reaches whose last fetch (or commit, if never
/// fetched) is older than `keep_hours`. The live set is the same `reachable`
/// walk the derivation GC keeps its rows by, so a path is never live while its
/// derivation is collectable and never collectable while its derivation lives.
pub async fn stale_cached_paths<C>(db: &C, keep_hours: i64) -> Result<Vec<String>, sea_orm::DbErr>
where
    C: ConnectionTrait
        + sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>
        + Sync,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(STALE_CACHED_PATHS.bind([sea_orm::Value::Int(Some(
            i32::try_from(keep_hours).unwrap_or(i32::MAX),
        ))]))
        .await?;
    walk.commit().await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect())
}

/// Deletes evaluations for `task_id`, retaining the most recent `keep`
/// terminal evaluations (see [`evaluations_to_gc`]).
///
/// Handles DB deletion, build log removal, NAR cache files, and GC root symlinks.
/// Skipped entirely while the task has any active evaluation, so an in-flight
/// run never loses NARs it is about to reuse.
pub async fn gc_task_evaluations(ctx: &DbContext, task_id: TaskId, keep: usize) -> Result<()> {
    if keep == 0 {
        return Ok(());
    }

    // Newest first; deletion selection counts only terminal evaluations.
    let all_evals = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task_id))
        .order_by_desc(CEvaluation::CreatedAt)
        .all(&ctx.worker_db)
        .await
        .context("GC: failed to query evaluations")?;

    let evals: Vec<(EvaluationStatus, chrono::NaiveDateTime)> = all_evals
        .iter()
        .map(|e| (e.status, last_progress_at(e)))
        .collect();
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
        task_id = %task_id,
        deleting = to_delete.len(),
        "Running per-task evaluation GC"
    );

    let lost = anchors_losing_an_evaluation(ctx, &to_delete).await?;

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

    // The evals' `build_job` rows cascade away, but their `build_attempt` rows
    // (and their logs) are set-null'd onto the surviving `derivation_build`
    // anchor - their true, build-once owner - and reclaimed only when the
    // derivation GC deletes that anchor. NAR files and GC roots are likewise
    // owned by `derivation_output` / `cache_derivation` and cleaned up there.
    //
    // Commits are not cascaded, so they are reclaimed afterwards in one pass:
    // deleting every evaluation first means the reference check sees the final
    // state, exactly as the per-evaluation check used to.
    let commit_ids: Vec<CommitId> = to_delete.iter().map(|e| e.commit).collect();

    for eval in &to_delete {
        let a: AEvaluation = eval.clone().into_active_model();
        a.delete(&ctx.worker_db)
            .await
            .context("GC: failed to delete evaluation")?;
    }

    let still_referenced: std::collections::HashSet<CommitId> = EEvaluation::find()
        .filter(CEvaluation::Commit.is_in(commit_ids.clone()))
        .all(&ctx.worker_db)
        .await
        .context("GC: failed to check commit references")?
        .into_iter()
        .map(|e| e.commit)
        .collect();

    let orphaned: Vec<CommitId> = commit_ids
        .into_iter()
        .filter(|c| !still_referenced.contains(c))
        .collect();

    if !orphaned.is_empty()
        && let Err(e) = ECommit::delete_many()
            .filter(CCommit::Id.is_in(orphaned))
            .exec(&ctx.worker_db)
            .await
    {
        warn!(error = %e, "GC: failed to delete orphaned commits");
    }

    let (adopted, changes) = settle_after_delete(ctx, &lost).await?;
    crate::status::emit_transition_effects(ctx, &changes).await;

    info!(task_id = %task_id, deleted = to_delete.len(), adopted, "Per-task evaluation GC done");
    Ok(())
}

/// Settle the queue after the deletions. A pruned subtree is named by the
/// evaluation that walked it and by nobody else, so its pending interior can lose
/// every name here while another evaluation still builds against it: the live
/// evaluations that reach it take the names over BEFORE the lost set is re-gated,
/// so nothing a live evaluation waits on leaves the queue, and what they adopted
/// is queued where its gates hold. Returns the adopted pair count and the moves.
async fn settle_after_delete(
    ctx: &DbContext,
    lost: &[DerivationId],
) -> Result<(usize, Vec<crate::status::TransitionChange>)> {
    let db = &ctx.worker_db;
    let adopted = if crate::reachability::pending_orphans_among(db, lost)
        .await
        .context("GC: failed to look for pending anchors the deletion left unnamed")?
    {
        crate::reachability::adopt_pending_closures(db)
            .await
            .context("GC: failed to hand the deleted evaluations' names to the live ones")?
    } else {
        crate::reachability::Adopted::default()
    };

    // Naming is half of what demand means, so a deletion can take it away and an
    // adoption can give it back: recompute below both before the queue is settled.
    let mut undemanded = Vec::new();
    let mut demanded = Vec::new();
    for chunk in lost
        .iter()
        .chain(adopted.derivations().iter())
        .copied()
        .collect::<Vec<_>>()
        .chunks(crate::IN_CHUNK_SIZE)
    {
        let moved = crate::readiness::recompute_demand(db, chunk)
            .await
            .context("GC: failed to recompute demand after deleting evaluations")?;
        undemanded.extend(moved.lost);
        demanded.extend(moved.gained);
    }

    let mut changes = Vec::new();
    for chunk in lost
        .iter()
        .chain(undemanded.iter())
        .copied()
        .collect::<Vec<_>>()
        .chunks(crate::IN_CHUNK_SIZE)
    {
        changes.extend(
            crate::readiness::unpromote_ungated(db, chunk)
                .await
                .context("GC: failed to settle the queue after deleting evaluations")?,
        );
    }

    for chunk in adopted
        .derivations()
        .iter()
        .chain(demanded.iter())
        .copied()
        .collect::<Vec<_>>()
        .chunks(crate::IN_CHUNK_SIZE)
    {
        changes.extend(
            crate::readiness::promote(db, chunk)
                .await
                .context("GC: failed to queue what the live evaluations adopted")?,
        );
    }

    crate::bump_graph_version(db, &adopted.evaluations())
        .await
        .context("GC: failed to bump the graph version of the adopting evaluations")?;

    Ok((adopted.pairs.len(), changes))
}

/// Every anchor whose queue membership the deletion of `evaluations` can close: the
/// derivations they name (a lost `build_job` or `entry_point` row) and the direct
/// inputs of those, which lose a demander with them.
///
/// Collected BEFORE the delete, because the rows it reads are what cascades away.
async fn anchors_losing_an_evaluation(
    ctx: &DbContext,
    evaluations: &[MEvaluation],
) -> Result<Vec<DerivationId>> {
    let ids: Vec<Uuid> = evaluations.iter().map(|e| e.id.into_inner()).collect();
    let rows = ctx
        .worker_db
        .query_all_raw(ANCHORS_LOSING_AN_EVALUATION.bind([ids.into()]))
        .await
        .context("GC: failed to collect the derivations of the evaluations to delete")?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect())
}

/// When an evaluation last advanced, as opposed to when its row was last
/// written. A wedged run keeps taking writes, so `updated_at` never goes stale
/// and the wedged escape hatch never fires for it; the phase stamps move only
/// when the evaluation enters a new phase, which a stuck one never does.
fn last_progress_at(e: &MEvaluation) -> chrono::NaiveDateTime {
    [
        e.fetch_started_at,
        e.eval_flake_started_at,
        e.eval_drv_started_at,
        e.building_started_at,
    ]
    .into_iter()
    .flatten()
    .fold(e.created_at, std::cmp::max)
}

/// Selects, by index into a newest-first evaluation list, which evaluations the
/// per-task GC should delete for a given `keep` count.
///
/// Returns nothing while any evaluation is genuinely active (Queued/Fetching/
/// Evaluating*/Building/Waiting): an in-flight run may reuse NARs from older
/// evaluations before it records its own build rows, so GC waits until the
/// task is quiescent. An "active" evaluation whose current phase has lasted
/// more than `wedged_hours` is presumed wedged and stops blocking - otherwise
/// one stuck run silently turns a scheduler bug into unbounded storage growth
/// (`wedged_hours = 0` restores the unconditional block). The age comes from
/// [`last_progress_at`], not `updated_at`, because a wedged run still takes
/// writes and so never looks stale. Wedged evaluations
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
/// The delete re-checks the orphan predicate inside the statement, because
/// derivations are global and content-addressed and a concurrent evaluation can
/// re-attach a `build_job` to a past-grace orphan at any moment, so a single
/// SELECT-then-delete would race the FK. Rows and attempt logs are all this pass
/// reclaims: the NARs of what it deletes leave the live set with it and are the
/// eviction pass's (`evict_stale_cached_paths`) to remove once past the fetch
/// TTL. FK cascade cleans up `derivation_output`, `derivation_build`, dep/closure
/// edges, features, metrics, and `cache_derivation`.
pub async fn gc_orphan_derivations(ctx: &DbContext, grace_hours: i64) -> Result<()> {
    use std::collections::HashSet;

    let cutoff = gradient_types::now() - ChronoDuration::hours(grace_hours.max(0));
    let db = &ctx.worker_db;

    let walk = crate::graph_sql::begin_walk(db)
        .await
        .context("GC: failed to open the keep-set walk")?;
    let rows = walk
        .query_all_raw(GC_ORPHAN_CANDIDATES.bind([sea_orm::Value::ChronoDateTime(Some(cutoff))]))
        .await
        .context("Failed to query orphan derivations")?;
    walk.commit()
        .await
        .context("GC: failed to close the keep-set walk")?;

    let candidate_ids: Vec<DerivationId> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "id").ok().map(DerivationId::new))
        .collect();

    if candidate_ids.is_empty() {
        return Ok(());
    }

    info!(count = candidate_ids.len(), "Running orphan derivation GC");

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

    // Snapshot the edges INTO the candidates before the delete, never after: the
    // `dependency` FK is ON DELETE CASCADE, so the rows naming a reclaimed
    // derivation are gone the moment it is, and a dependent that survived it has
    // lost part of its record and must be re-walked.
    let mut dependents: Vec<(Uuid, Uuid)> = Vec::new();
    for chunk in candidate_ids.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Vec<Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        dependents.extend(
            db.query_all_raw(ORPHAN_DEPENDENTS.bind([ids.into()]))
                .await
                .context("GC: failed to query dependents of the candidates")?
                .into_iter()
                .filter_map(|r| {
                    Some((
                        r.try_get::<Uuid>("", "derivation").ok()?,
                        r.try_get::<Uuid>("", "dependency").ok()?,
                    ))
                }),
        );
    }

    let mut deleted: HashSet<DerivationId> = HashSet::new();
    for chunk in candidate_ids.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Vec<Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        // The re-check walks the whole keep-set again, so the chunk gets its own
        // walk transaction; a failed chunk rolls its delete back and is skipped.
        let outcome = async {
            let walk = crate::graph_sql::begin_walk(db).await?;
            let returned = walk
                .query_all_raw(GC_ORPHAN_DELETE.bind([ids.into()]))
                .await?;
            walk.commit().await?;
            Ok::<Vec<sea_orm::QueryResult>, sea_orm::DbErr>(returned)
        }
        .await;

        match outcome {
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

    // A surviving dependent's record lost an edge, so `walked` is no longer true
    // of it and every gate that reads it must close until a fresh evaluation
    // re-walks it. The un-promote re-checks the gates rather than the list, so a
    // dependent a concurrent eval already re-walked keeps its place in the queue.
    let orphaned = orphaned_survivors(&dependents, &deleted);
    if !orphaned.is_empty() {
        let survivors: Vec<DerivationId> =
            orphaned.iter().copied().map(DerivationId::new).collect();
        let settled = async {
            use sea_orm::TransactionTrait;
            let txn = db.begin().await?;
            txn.execute_raw(UNWALK_ORPHAN_SURVIVORS.bind([orphaned.into()]))
                .await?;
            let changes = crate::readiness::unpromote_ungated(&txn, &survivors).await?;
            txn.commit().await?;
            Ok::<_, sea_orm::DbErr>(changes)
        }
        .await;
        match settled {
            Ok(changes) => crate::status::emit_transition_effects(ctx, &changes).await,
            Err(e) => {
                warn!(error = %e, "GC: failed to re-walk the survivors of a deleted dependency")
            }
        }
    }

    // Reclaim the log files of every attempt whose derivation was just deleted;
    // their `build_attempt`/`build_log_chunk` rows already cascaded away.
    for attempt_id in attempt_logs_to_reclaim(&attempt_snapshot, &deleted) {
        if let Err(e) = ctx.storage.log_storage.delete(attempt_id).await {
            warn!(error = %e, %attempt_id, "GC: failed to remove orphan build log");
        }
    }

    info!(deleted = deleted.len(), "Orphan derivation GC done");
    Ok(())
}

/// The derivations that SURVIVED the sweep while at least one of their
/// dependencies was reclaimed, sorted and deduplicated. Their record is now
/// incomplete - the edge went with the dependency - so they are exactly the rows
/// that must lose `walked`. A dependent that was itself deleted is excluded: its
/// row is gone and updating it would be a no-op on a cascaded id.
fn orphaned_survivors(
    dependents: &[(Uuid, Uuid)],
    deleted: &std::collections::HashSet<DerivationId>,
) -> Vec<Uuid> {
    let mut survivors: Vec<Uuid> = dependents
        .iter()
        .filter(|(dependent, dependency)| {
            deleted.contains(&DerivationId::new(*dependency))
                && !deleted.contains(&DerivationId::new(*dependent))
        })
        .map(|(dependent, _)| *dependent)
        .collect();
    survivors.sort_unstable();
    survivors.dedup();

    survivors
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

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Stale means outside the live set and untouched (fetched or committed)
    /// for `keep_hours`; the bound is one parameter.
    #[tokio::test]
    async fn stale_cached_paths_selects_outside_the_live_set_past_the_bound() {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![BTreeMap::from([(
                "hash".to_owned(),
                Value::String(Some("abc".into())),
            )])]])
            .into_connection();

        let stale = stale_cached_paths(&db, 336).await.unwrap();

        assert_eq!(stale, vec!["abc".to_owned()]);
        let log = db.into_transaction_log();
        let stmt = log[0]
            .statements()
            .iter()
            .find(|s| s.sql.contains("FROM cached_path cp"))
            .expect("the walk issues the stale selection");
        let sql = norm(&stmt.sql);
        assert!(
            sql.contains("WHERE NOT EXISTS (SELECT 1 FROM live l WHERE l.hash = cp.hash)"),
            "{sql}"
        );
        assert!(
            sql.contains(concat!(
                "coalesce((SELECT max(s.last_fetched_at) FROM cached_path_signature s ",
                "WHERE s.cached_path = cp.id), cp.created_at) < ",
                "(now() AT TIME ZONE 'UTC') - make_interval(hours => $1::int)",
            )),
            "{sql}"
        );
        assert_eq!(stmt.values.as_ref().map(|v| v.0.len()), Some(1));
    }

    /// A dependent that survives the sweep while one of its dependencies is
    /// reclaimed has an incomplete record: it must be re-walked, so it is
    /// exactly the survivors of deleted dependencies that lose `walked`. A
    /// dependent of a kept dependency, and one that was itself deleted, are not.
    #[test]
    fn survivors_of_a_deleted_dependency_lose_walked() {
        let gone = DerivationId::now_v7();
        let kept = DerivationId::now_v7();
        let survivor = DerivationId::now_v7();
        let also_gone = DerivationId::now_v7();
        let dependents = vec![
            (survivor.into_inner(), gone.into_inner()),
            (survivor.into_inner(), kept.into_inner()),
            (also_gone.into_inner(), gone.into_inner()),
        ];
        let deleted: HashSet<DerivationId> = [gone, also_gone].into_iter().collect();

        assert_eq!(
            orphaned_survivors(&dependents, &deleted),
            vec![survivor.into_inner()]
        );
    }

    /// Nothing deleted is nothing to re-walk: an empty deleted set must not read
    /// as "every dependent lost an edge".
    #[test]
    fn no_deletion_orphans_nobody() {
        let a = DerivationId::now_v7();
        let b = DerivationId::now_v7();
        let dependents = vec![(a.into_inner(), b.into_inner())];

        assert!(orphaned_survivors(&dependents, &HashSet::new()).is_empty());
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

    const WEDGED_HOURS: i64 = 24;

    fn at(
        statuses: &[EvaluationStatus],
        age_hours: i64,
    ) -> Vec<(EvaluationStatus, chrono::NaiveDateTime)> {
        let progressed = gradient_types::now() - ChronoDuration::hours(age_hours);
        statuses.iter().map(|s| (*s, progressed)).collect()
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
        // freezes the task's GC; terminal evals beyond keep are reclaimed,
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
    fn a_heartbeating_wedged_evaluation_still_goes_stale() {
        // #609: `updated_at` is refreshed by anything that writes the row, so a
        // run stuck in Building for days never crossed the threshold and froze
        // its task's GC forever. Measured on the phase stamp it does cross.
        let now = gradient_types::now();
        let wedged = MEvaluation {
            created_at: now - ChronoDuration::hours(72),
            building_started_at: Some(now - ChronoDuration::hours(71)),
            updated_at: now - ChronoDuration::minutes(39),
            ..Default::default()
        };

        assert_eq!(last_progress_at(&wedged), now - ChronoDuration::hours(71));
        assert!(now - last_progress_at(&wedged) > ChronoDuration::hours(WEDGED_HOURS));
        assert!(now - wedged.updated_at < ChronoDuration::hours(WEDGED_HOURS));
    }

    #[test]
    fn entering_a_phase_is_progress_and_keeps_the_block() {
        // The converse, so the fix cannot be read as "active evals stop
        // blocking after a day": a run that reached a new phase an hour ago is
        // advancing, however old its earlier stamps are.
        let now = gradient_types::now();
        let moving = MEvaluation {
            created_at: now - ChronoDuration::hours(72),
            fetch_started_at: Some(now - ChronoDuration::hours(71)),
            eval_flake_started_at: Some(now - ChronoDuration::hours(70)),
            building_started_at: Some(now - ChronoDuration::hours(1)),
            updated_at: now,
            ..Default::default()
        };

        assert_eq!(last_progress_at(&moving), now - ChronoDuration::hours(1));
    }

    #[test]
    fn an_evaluation_with_no_phase_stamp_ages_from_its_creation() {
        // A `Queued` run has entered no phase, so creation is the only honest
        // mark of when it last moved.
        let now = gradient_types::now();
        let queued = MEvaluation {
            created_at: now - ChronoDuration::hours(48),
            updated_at: now,
            ..Default::default()
        };

        assert_eq!(last_progress_at(&queued), now - ChronoDuration::hours(48));
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

    fn exec(rows_affected: u64) -> sea_orm::MockExecResult {
        sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    /// The names go over before the queue is settled, so an interior a live
    /// evaluation still builds against never leaves the queue in between; what
    /// was adopted is then queued where its gates hold, and the adopting
    /// evaluations' graph version moves because their build list did.
    #[tokio::test]
    async fn the_live_evaluations_adopt_before_the_lost_names_are_regated() {
        use sea_orm::{DatabaseBackend, MockDatabase, Value};
        use std::collections::BTreeMap;

        let live = EvaluationId::now_v7();
        let orphan = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "?column?".to_owned(),
                Value::Int(Some(1)),
            )])]])
            .append_exec_results([exec(0)])
            .append_query_results([vec![BTreeMap::from([
                ("evaluation".to_owned(), Value::from(live.into_inner())),
                ("derivation".to_owned(), Value::from(orphan.into_inner())),
            ])]])
            .append_query_results([empty.clone(), empty])
            .append_exec_results([exec(1)])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let (adopted, changes) = settle_after_delete(&ctx, &[orphan]).await.unwrap();
        drop(ctx);

        assert_eq!(adopted, 1);
        assert!(changes.is_empty());
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 6, "{log:?}");
        assert!(
            log[0].contains(
                "NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) LIMIT 1"
            ),
            "the GC asks before it walks: {log:?}"
        );
        assert!(
            log[1].contains("SET LOCAL work_mem") && log[2].contains("INSERT INTO build_job"),
            "the walk runs under its own raise: {log:?}"
        );
        assert!(
            log[3].contains("SET status = 0") && log[3].contains("db.derivation = ANY($1::uuid[])"),
            "the lost set is re-gated only after the names moved: {log:?}"
        );
        assert!(
            log[4].contains("SET status = 1"),
            "what was adopted is queued where its gates hold: {log:?}"
        );
        assert!(
            log[5].contains("graph_version = e.graph_version + 1"),
            "{log:?}"
        );
    }

    /// Nothing pending lost its last name: no walk is paid for, and the lost set
    /// is re-gated as before.
    #[tokio::test]
    async fn a_deletion_that_orphans_nothing_pending_walks_nothing() {
        use sea_orm::{DatabaseBackend, MockDatabase, Value};
        use std::collections::BTreeMap;

        let d = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([empty.clone(), empty])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let (adopted, changes) = settle_after_delete(&ctx, &[d]).await.unwrap();
        drop(ctx);

        assert_eq!(adopted, 0);
        assert!(changes.is_empty());
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(
            log[0].contains("LIMIT 1") && log[1].contains("SET status = 0"),
            "{log:?}"
        );
    }
}
