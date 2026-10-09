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

use crate::DbContext;
use gradient_types::*;

fn gc_orphan_candidates_sql() -> String {
    format!(
        "{reachable}
         SELECT d.id FROM derivation d
         WHERE d.created_at < $1
           AND NOT EXISTS (SELECT 1 FROM reachable rc WHERE rc.derivation = d.id)",
        reachable = crate::graph::walks::reachable_derivations_cte(),
    )
}

crate::sql_fn! {
    GC_ORPHAN_CANDIDATES = gc_orphan_candidates_sql,
        params = [Now],
        tier = Sweep,
        flags = [Walk];
}

fn expired_cached_paths_sql() -> String {
    format!(
        "{live}
         SELECT cp.hash FROM cached_path cp
         WHERE NOT EXISTS (SELECT 1 FROM live l WHERE l.hash = cp.hash)
           AND coalesce((SELECT max(s.last_fetched_at) FROM cached_path_signature s
                         WHERE s.cached_path = cp.id), cp.created_at)
               < (now() AT TIME ZONE 'UTC') - make_interval(hours => $1::int)",
        live = crate::graph::walks::live_cached_paths_cte(),
    )
}

crate::sql_fn! {
    EXPIRED_CACHED_PATHS = expired_cached_paths_sql,
        params = [Int(336)],
        tier = Sweep,
        budget = crate::sql::Budget::sweep().buffers(1_500_000)
            .because("the collector walks every live path before it can name a dead one"),
        flags = [Walk];
}

pub async fn expired_cached_paths<C>(db: &C, keep_hours: i64) -> Result<Vec<String>, sea_orm::DbErr>
where
    C: ConnectionTrait
        + sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>
        + Sync,
{
    let walk = crate::graph::walks::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(EXPIRED_CACHED_PATHS.bind([sea_orm::Value::Int(Some(
            i32::try_from(keep_hours).unwrap_or(i32::MAX),
        ))]))
        .await?;
    walk.commit().await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect())
}

pub async fn evaluation_gc_plan(
    ctx: &DbContext,
    task_id: TaskId,
    keep: usize,
) -> Result<Vec<MEvaluation>> {
    if keep == 0 {
        return Ok(Vec::new());
    }

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
        ctx.config.gc.wedged_eval_hours,
        gradient_types::now(),
    )
    .into_iter()
    .map(|i| all_evals[i].clone())
    .collect())
}

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

pub async fn settle_after_delete(
    ctx: &DbContext,
    lost: &[DerivationId],
) -> Result<(usize, Vec<crate::status::TransitionChange>)> {
    let db = &ctx.worker_db;
    let adopted = if crate::graph::reachability::pending_orphans_among(db, lost)
        .await
        .context("GC: failed to look for pending shared builds the deletion left unnamed")?
    {
        crate::graph::reachability::adopt_pending_closures(db)
            .await
            .context("GC: failed to hand the deleted evaluations' names to the live ones")?
    } else {
        crate::graph::reachability::Adopted::default()
    };

    let mut changes = Vec::new();
    for chunk in lost
        .iter()
        .chain(adopted.derivations().iter())
        .copied()
        .collect::<Vec<_>>()
        .chunks(crate::IN_CHUNK_SIZE)
    {
        changes.extend(
            crate::graph::can_start::update_and_settle_need(db, chunk)
                .await
                .context("GC: failed to settle need after deleting evaluations")?
                .changes,
        );
    }

    for chunk in lost.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(
            crate::graph::can_start::unpromote_ungated(db, chunk)
                .await
                .context("GC: failed to settle the queue after deleting evaluations")?,
        );
    }

    for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(
            crate::graph::can_start::promote(db, chunk)
                .await
                .context("GC: failed to queue what the live evaluations adopted")?,
        );
    }

    crate::task_board::dep_counts::bump_graph_version(db, &adopted.evaluations())
        .await
        .context("GC: failed to bump the graph version of the adopting evaluations")?;

    Ok((adopted.pairs.len(), changes))
}

/// A wedged run is still taking writes, and `updated_at` is never going stale for it.
/// The phase stamps are only moving when the evaluation is entering a new phase.
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

/// The returned timestamp is taken before the walk.
/// The graph writer's delete is re-checking exactly what became live since then.
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

    let walk = crate::graph::walks::begin_walk(db).await?;
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

    #[tokio::test]
    async fn expired_cached_paths_selects_outside_the_live_set_past_the_bound() {
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

        let stale = expired_cached_paths(&db, 336).await.unwrap();

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
        assert!(gc(&[Building, Completed], 1).is_empty());
        assert!(gc(&[Queued, Building, Waiting, Fetching], 1).is_empty());
        assert!(gc(&[Building, Completed, Aborted, Completed], 1).is_empty());
    }

    #[test]
    fn wedged_active_evaluation_stops_blocking_but_is_never_deleted() {
        let evals = at(&[Building, Completed, Failed, Completed], 48);
        assert_eq!(
            evaluations_to_gc(&evals, 1, WEDGED_HOURS, gradient_types::now()),
            vec![2, 3]
        );
        assert!(evaluations_to_gc(&evals, 1, 0, gradient_types::now()).is_empty());
    }

    #[test]
    fn a_heartbeating_wedged_evaluation_still_goes_stale() {
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
                && log[4].contains("ORDER BY derivation FOR NO KEY UPDATE")
                && log[5].contains("ON d.derivation = r.derivation ORDER BY r.derivation"),
            "what lost a name and what gained one are updated together, locked and raised: {log:?}"
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
                && log[3].contains("ON d.derivation = r.derivation ORDER BY r.derivation")
                && !log.iter().any(|s| s.contains("SET wanted ="))
                && log[4].contains("SET status = 0"),
            "{log:?}"
        );
    }
}
