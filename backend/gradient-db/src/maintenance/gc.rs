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
    let walk = crate::graph::walks::begin_walk(db, &EXPIRED_CACHED_PATHS).await?;
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
        .context("GC: query evaluations")?;

    let statuses: Vec<EvaluationStatus> = all_evals.iter().map(|e| e.status).collect();

    Ok(evaluations_to_gc(&statuses, keep)
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
        .context("GC: check commit references")?
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
        .context("GC: look for pending shared builds the deletion left unnamed")?
    {
        crate::graph::reachability::adopt_pending_closures(db)
            .await
            .context("GC: hand the deleted evaluations' names to the live ones")?
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
                .context("GC: settle need after deleting evaluations")?
                .changes,
        );
    }

    for chunk in lost.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(
            crate::graph::can_start::unpromote_ungated(db, chunk)
                .await
                .context("GC: settle the queue after deleting evaluations")?,
        );
    }

    for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(
            crate::graph::can_start::promote(db, chunk)
                .await
                .context("GC: queue what the live evaluations adopted")?,
        );
    }

    crate::task_board::dep_counts::bump_graph_version(db, &adopted.evaluations())
        .await
        .context("GC: bump the graph version of the adopting evaluations")?;

    Ok((adopted.pairs.len(), changes))
}

fn evaluations_to_gc(newest_first: &[EvaluationStatus], keep: usize) -> Vec<usize> {
    let mut terminal = (0..newest_first.len()).filter(|&i| newest_first[i].is_terminal());
    // Cleanup jobs never remove the latest terminal evaluation, even when it failed. The task page can still show the latest result.
    if keep == 0 || terminal.next().is_none() {
        return Vec::new();
    }

    let (completed, unsuccessful): (Vec<usize>, Vec<usize>) =
        terminal.partition(|&i| newest_first[i] == EvaluationStatus::Completed);
    let mut room = keep - 1;
    let mut delete = Vec::new();
    for older in [completed, unsuccessful] {
        let kept = older.len().min(room);
        room -= kept;
        delete.extend_from_slice(&older[kept..]);
    }

    delete.sort_unstable();
    delete
}

pub async fn orphan_derivation_candidates<C>(
    db: &C,
    grace_hours: i64,
) -> Result<Vec<DerivationId>, sea_orm::DbErr>
where
    C: ConnectionTrait
        + sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>
        + Sync,
{
    let cutoff = gradient_types::now() - ChronoDuration::hours(grace_hours.max(0));

    let walk = crate::graph::walks::begin_walk(db, &GC_ORPHAN_CANDIDATES).await?;
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

    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use EvaluationStatus::*;
    use gradient_fixtures::mock::{mock_db, statements};

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[tokio::test]
    async fn expired_cached_paths_selects_outside_the_live_set_past_the_bound() {
        use sea_orm::{MockExecResult, Value};
        use std::collections::BTreeMap;

        let db = mock_db()
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

    use super::evaluations_to_gc as gc;

    #[test]
    fn active_evaluations_are_never_deleted() {
        assert!(gc(&[Queued, Building, Waiting, Fetching], 1).is_empty());
        assert!(gc(&[EvaluatingFlake, EvaluatingDerivation], 1).is_empty());
    }

    #[test]
    fn an_active_evaluation_neither_blocks_gc_nor_counts_toward_keep() {
        assert_eq!(gc(&[Building, Completed, Failed, Completed], 2), vec![2]);
        assert_eq!(gc(&[Completed, Building, Completed, Completed], 2), vec![3]);
    }

    #[test]
    fn failed_and_aborted_evaluations_go_before_completed_ones() {
        assert_eq!(
            gc(&[Completed, Failed, Aborted, Completed, Completed], 3),
            vec![1, 2]
        );
    }

    #[test]
    fn the_newest_terminal_evaluation_is_kept_even_when_it_failed() {
        assert_eq!(gc(&[Failed, Completed, Completed, Failed], 2), vec![2, 3]);
        assert_eq!(gc(&[Building, Aborted, Completed], 1), vec![2]);
    }

    #[test]
    fn failures_fill_the_rest_when_too_few_evaluations_completed() {
        assert_eq!(
            gc(&[Failed, Aborted, Completed, Failed, Failed], 4),
            vec![4]
        );
    }

    #[test]
    fn a_task_with_steady_input_updates_shrinks_to_keep() {
        let mut task = vec![Building, Failed];
        task.extend([Failed; 30]);
        task.extend([Completed; 5]);
        task.push(Aborted);

        let survivors: Vec<EvaluationStatus> = (0..task.len())
            .filter(|i| !gc(&task, 3).contains(i))
            .map(|i| task[i])
            .collect();

        assert_eq!(survivors, vec![Building, Failed, Completed, Completed]);
    }

    #[test]
    fn evaluations_within_keep_stay() {
        assert!(gc(&[Aborted], 1).is_empty());
        assert!(gc(&[Completed, Failed, Aborted], 3).is_empty());
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
        use sea_orm::Value;
        use std::collections::BTreeMap;

        let live = EvaluationId::now_v7();
        let orphan = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = mock_db()
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
        let log = statements(pool.into_transaction_log());
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
        use sea_orm::Value;
        use std::collections::BTreeMap;

        let d = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = mock_db()
            .append_query_results([empty.clone(), empty.clone(), empty])
            .append_exec_results([exec(0), exec(0)])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let (adopted, changes) = settle_after_delete(&ctx, &[d]).await.unwrap();
        drop(ctx);

        assert_eq!(adopted, 0);
        assert!(changes.is_empty());
        let log = statements(pool.into_transaction_log());
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
