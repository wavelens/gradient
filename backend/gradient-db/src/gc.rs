/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::Duration as ChronoDuration;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use tracing::{info, warn};
use uuid::Uuid;

use super::DbContext;
use gradient_types::*;

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

/// The evaluations of `task_id` this pass should delete, retaining the most
/// recent `keep` terminal ones (see [`evaluations_to_gc`]).
///
/// Selection only: the rows are deleted by the graph actor, which owns every
/// write to the graph, and the log files and orphaned commits are reclaimed by
/// [`after_evaluation_delete`] once the actor reports what it removed.
pub async fn evaluation_gc_plan(
    ctx: &DbContext,
    task_id: TaskId,
    keep: usize,
) -> Result<Vec<MEvaluation>> {
    if keep == 0 {
        return Ok(Vec::new());
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

    Ok(evaluations_to_gc(
        &evals,
        keep,
        ctx.config.storage.gc_wedged_eval_hours,
        gradient_types::now(),
    )
    .into_iter()
    .map(|i| all_evals[i].clone())
    .collect())
}

/// What the evaluation delete leaves behind outside the graph: commits nothing
/// references any more. The `build_job` rows cascaded with their evaluation, and
/// the `build_attempt` rows were set-null'd onto the surviving `derivation_build`
/// anchor - their true, build-once owner - so their logs are the derivation GC's
/// to reclaim, not this pass's.
pub async fn after_evaluation_delete(ctx: &DbContext, deleted: &[MEvaluation]) -> Result<()> {
    if deleted.is_empty() {
        return Ok(());
    }

    let commit_ids: Vec<CommitId> = deleted.iter().map(|e| e.commit).collect();
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

    Ok(())
}

/// Settle the queue after the deletions. A pruned subtree is named by the
/// evaluation that walked it and by nobody else, so its pending interior can lose
/// every name here while another evaluation still builds against it: the live
/// evaluations that reach it take the names over BEFORE the lost set is re-gated,
/// so nothing a live evaluation waits on leaves the queue, and what they adopted
/// is queued where its gates hold. Returns the adopted pair count and the moves.
pub async fn settle_after_delete(
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

/// Derivation GC candidate scan (the mark half of mark-and-sweep): global
/// `derivation` rows that lie *outside the build-dependency closure of every live
/// root* - an `entry_point` or a derivation a retained eval's `build_job`
/// references - and whose grace period has expired. The grace lets rapid
/// re-evaluations reuse recent derivations.
///
/// Reachability matters because `build_job` rows are pruned with old evals while
/// `derivation_dependency` edges and anchors persist: a derivation still needed as
/// a build input of a retained closure (its own evals long gone) has no `build_job`
/// yet must be kept. A naive "no `build_job`" test reclaimed those, deleting build
/// inputs of live anchors and stranding dependents on `InputsUnavailable`.
///
/// Read-only, and run on the pool: the whole keep-set walk is too long to hold the
/// graph actor's single writer lock. The returned timestamp is taken BEFORE the
/// walk, so the actor's delete can re-check exactly what became live since - see
/// `gradient_graph::gc`. Rows and attempt logs are all this pass reclaims: the
/// NARs of what it deletes leave the live set with it and are the eviction pass's
/// (`evict_stale_cached_paths`) to remove once past the fetch TTL. FK cascade
/// cleans up `derivation_output`, `derivation_build`, dep/closure edges, features,
/// metrics, and `cache_derivation`.
pub async fn orphan_derivation_candidates<C>(
    db: &C,
    grace_hours: i64,
) -> Result<(Vec<DerivationId>, chrono::NaiveDateTime), sea_orm::DbErr>
where
    C: ConnectionTrait
        + sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>
        + Sync,
{
    let scanned_at = gradient_types::now();
    let cutoff = scanned_at - ChronoDuration::hours(grace_hours.max(0));

    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(GC_ORPHAN_CANDIDATES.bind([sea_orm::Value::ChronoDateTime(Some(cutoff))]))
        .await?;
    walk.commit().await?;

    let candidates: Vec<DerivationId> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "id").ok().map(DerivationId::new))
        .collect();
    if !candidates.is_empty() {
        info!(count = candidates.len(), "Running orphan derivation GC");
    }

    Ok((candidates, scanned_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use EvaluationStatus::*;

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
            .append_exec_results([exec(0), exec(0)])
            .append_query_results([empty.clone(), empty.clone(), empty])
            .append_exec_results([exec(1)])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let (adopted, changes) = settle_after_delete(&ctx, &[orphan]).await.unwrap();
        drop(ctx);

        assert_eq!(adopted, 1);
        assert!(changes.is_empty());
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 9, "{log:?}");
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
            log[3].contains("SET LOCAL work_mem")
                && log[4].contains("ORDER BY derivation FOR UPDATE")
                && log[5].contains("SET demanded ="),
            "what lost a name and what gained one are recomputed together, locked and raised: {log:?}"
        );
        assert!(
            log[6].contains("SET status = 0") && log[6].contains("db.derivation = ANY($1::uuid[])"),
            "the lost set is re-gated only after the names moved: {log:?}"
        );
        assert!(
            log[7].contains("SET status = 1"),
            "what was adopted is queued where its gates hold: {log:?}"
        );
        assert!(
            log[8].contains("graph_version = e.graph_version + 1"),
            "{log:?}"
        );
    }

    /// Nothing pending lost its last name: no adoption walk is paid for, and the
    /// lost set is recomputed and re-gated as before.
    #[tokio::test]
    async fn a_deletion_that_orphans_nothing_pending_walks_nothing() {
        use sea_orm::{DatabaseBackend, MockDatabase, Value};
        use std::collections::BTreeMap;

        let d = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([empty.clone(), empty.clone(), empty])
            .append_exec_results([exec(0), exec(0)])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let (adopted, changes) = settle_after_delete(&ctx, &[d]).await.unwrap();
        drop(ctx);

        assert_eq!(adopted, 0);
        assert!(changes.is_empty());
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 5, "{log:?}");
        assert!(
            log[0].contains("LIMIT 1")
                && !log.iter().any(|s| s.contains("INSERT INTO build_job"))
                && log[3].contains("SET demanded =")
                && log[4].contains("SET status = 0"),
            "{log:?}"
        );
    }
}
