/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The sweep is the only backstop for the moved columns.
//! `GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS = 0` is leaving them with none.
//! A table-wide recount runs only after its drift check finds a counter its direct inputs contradict.
//! Skipping it is only correct without cycles: on a cycle, stuck counters agree with their inputs
//! and the recount never runs to fix them.

use crate::graph::can_start::{SWEEP_CHUNKS_PER_TRANSACTION, SWEEP_LOCK_CHUNK};
use crate::graph::reachability::Adopted;
use crate::{DbContext, WorkerDb};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_continuation::ContinuationStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DbErr, Statement};
use std::ops::Range;

const SWEEP_MAX_LOCK_KEYS: usize = SWEEP_LOCK_CHUNK * SWEEP_CHUNKS_PER_TRANSACTION;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConsistencyReport {
    pub counter_drift: i64,
    pub walk_drift: i64,
    pub runtime_drift: i64,
    pub need_drift: i64,
    pub skipped_moves: i64,
    pub unpromoted_startable: i64,
    pub adopted: i64,
    pub unbacked_trusted_outputs: i64,
    pub wedged_building_evals: i64,
    pub eval_counter_drift: i64,
    pub repair_scope: i64,
}

impl ConsistencyReport {
    pub fn total(&self) -> i64 {
        self.counter_drift
            + self.walk_drift
            + self.runtime_drift
            + self.need_drift
            + self.skipped_moves
            + self.unpromoted_startable
            + self.unbacked_trusted_outputs
            + self.wedged_building_evals
            + self.eval_counter_drift
            + self.adopted
    }
}

fn unbacked_trusted_output_count_sql() -> String {
    format!(
        "SELECT count(*) AS n FROM ({}) u",
        crate::caches::demotion::unbacked_trusted_outputs_select()
    )
}

crate::sql_fn! {
    UNBACKED_TRUSTED_OUTPUT_COUNT = unbacked_trusted_output_count_sql,
        params = [],
        tier = Sweep;
}

fn wedged_building_evals_sql() -> String {
    format!(
        "SELECT count(*) AS n FROM evaluation ev \
         WHERE ev.status = {building} AND ev.active_shared_builds = 0 \
         AND NOT EXISTS (SELECT 1 FROM exec_job ej \
                         WHERE ej.evaluation = ev.id AND ej.status IN ({open_exec}) \
                         AND NOT ej.waits_for_manual_start) \
         AND NOT EXISTS (SELECT 1 FROM exec_job_continuation c \
                         WHERE c.evaluation = ev.id AND c.status = {queued})",
        building = crate::sql::status::eval(EvaluationStatus::Building),
        open_exec = crate::sql::status::exec_in(&ExecJobStatus::OPEN),
        queued = i16::from(ContinuationStatus::Queued),
    )
}

crate::sql_fn! {
    WEDGED_BUILDING_EVALS = wedged_building_evals_sql,
        params = [],
        tier = Sweep;
}

async fn count<C: ConnectionTrait>(db: &C, stmt: Statement) -> Result<i64, DbErr> {
    let row = db.query_one_raw(stmt).await?;
    Ok(row
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildGraphConsistencyCheckStep {
    RecountUnwalkedInputs,
    RecountMissingRuntimeDeps,
    ReadRepairScope,
    RepairFetchable,
    RecountWanted,
    SettleSkipped,
    RecountBlockingDeps,
    UnpromoteAndPromote,
    AdoptPendingClosures,
    CountAlarms,
}

impl BuildGraphConsistencyCheckStep {
    const ALL_CONSISTENCY_CHECK_STEPS_IN_RUN_ORDER: [Self; 10] = [
        Self::RecountUnwalkedInputs,
        Self::RecountMissingRuntimeDeps,
        Self::ReadRepairScope,
        Self::RepairFetchable,
        Self::RecountWanted,
        Self::SettleSkipped,
        Self::RecountBlockingDeps,
        Self::UnpromoteAndPromote,
        Self::AdoptPendingClosures,
        Self::CountAlarms,
    ];

    pub const fn step_name(self) -> &'static str {
        match self {
            Self::RecountUnwalkedInputs => "recount_unwalked_inputs",
            Self::RecountMissingRuntimeDeps => "recount_missing_runtime_deps",
            Self::ReadRepairScope => "read_repair_scope",
            Self::RepairFetchable => "repair_fetchable",
            Self::RecountWanted => "recount_wanted",
            Self::SettleSkipped => "settle_skipped",
            Self::RecountBlockingDeps => "recount_blocking_deps",
            Self::UnpromoteAndPromote => "unpromote_and_promote",
            Self::AdoptPendingClosures => "adopt_pending_closures",
            Self::CountAlarms => "count_alarms",
        }
    }

    fn next_step_in_run_order(self) -> Option<Self> {
        Self::ALL_CONSISTENCY_CHECK_STEPS_IN_RUN_ORDER
            .into_iter()
            .skip_while(|step| *step != self)
            .nth(1)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsistencyCheckRepairScopeChunkProgress {
    covered_derivation_ids: Vec<DerivationId>,
    pending_adopted_ids: Option<Vec<DerivationId>>,
    derivation_ids_pending_forgotten_probe_rewalk: Vec<DerivationId>,
    derivation_ids_waiting_for_promotion_after_rewalks: Vec<DerivationId>,
    next_transaction_start_index: usize,
    report_so_far: ConsistencyReport,
}

impl ConsistencyCheckRepairScopeChunkProgress {
    fn next_step_after_finished_step(
        &self,
        finished: BuildGraphConsistencyCheckStep,
    ) -> Option<BuildGraphConsistencyCheckStep> {
        if self.chunked_step_needs_another_message(finished) {
            return Some(finished);
        }

        finished.next_step_in_run_order()
    }

    fn reset_chunk_start_index_and_get_next_step_after_lock_timeout_failed_step(
        &mut self,
        failed: BuildGraphConsistencyCheckStep,
    ) -> Option<BuildGraphConsistencyCheckStep> {
        self.next_transaction_start_index = 0;
        failed.next_step_in_run_order()
    }

    fn chunked_step_needs_another_message(&self, step: BuildGraphConsistencyCheckStep) -> bool {
        use BuildGraphConsistencyCheckStep as Step;

        match step {
            Step::RepairFetchable | Step::RecountBlockingDeps => {
                self.next_transaction_start_index != 0
            }
            Step::AdoptPendingClosures => {
                self.next_transaction_start_index != 0
                    || !self
                        .derivation_ids_pending_forgotten_probe_rewalk
                        .is_empty()
            }
            _ => false,
        }
    }

    fn take_next_slice_range(&mut self, len: usize) -> Range<usize> {
        let start = self.next_transaction_start_index;
        let end = (start + SWEEP_MAX_LOCK_KEYS).min(len);
        self.next_transaction_start_index = if end < len { end } else { 0 };
        start..end
    }
}

pub async fn run_consistency_check_steps<E, F, Fut>(
    is_postgres_lock_timeout_error: impl Fn(&E) -> bool,
    mut run_step: F,
) -> Result<ConsistencyReport, E>
where
    E: std::fmt::Display,
    F: FnMut(BuildGraphConsistencyCheckStep, ConsistencyCheckRepairScopeChunkProgress) -> Fut,
    Fut: Future<Output = Result<ConsistencyCheckRepairScopeChunkProgress, E>>,
{
    let mut progress = ConsistencyCheckRepairScopeChunkProgress::default();
    let mut step =
        Some(BuildGraphConsistencyCheckStep::ALL_CONSISTENCY_CHECK_STEPS_IN_RUN_ORDER[0]);
    while let Some(current) = step {
        step = match run_step(current, progress.clone()).await {
            Ok(finished) => {
                progress = finished;
                progress.next_step_after_finished_step(current)
            }
            Err(e) if is_postgres_lock_timeout_error(&e) => {
                tracing::warn!(
                    step = current.step_name(),
                    error = %format_args!("{e:#}"),
                    "consistency check step hit a lock timeout. skipping to the next step"
                );
                progress.reset_chunk_start_index_and_get_next_step_after_lock_timeout_failed_step(
                    current,
                )
            }
            Err(e) => return Err(e),
        };
    }

    Ok(progress.report_so_far)
}

pub async fn graph_consistency_report(ctx: &DbContext) -> Result<ConsistencyReport, DbErr> {
    run_consistency_check_steps(
        crate::graph::can_start::lock_unavailable,
        |step, progress| run_one_consistency_check_step(ctx, step, progress),
    )
    .await
}

pub async fn run_one_consistency_check_step(
    ctx: &DbContext,
    step: BuildGraphConsistencyCheckStep,
    mut progress: ConsistencyCheckRepairScopeChunkProgress,
) -> Result<ConsistencyCheckRepairScopeChunkProgress, DbErr> {
    use BuildGraphConsistencyCheckStep as Step;

    let db = &ctx.worker_db;
    let report = &mut progress.report_so_far;
    match step {
        Step::RecountUnwalkedInputs => {
            report.walk_drift =
                crate::graph::walk_completeness::recount_walk_completeness(db).await? as i64;
        }
        Step::RecountMissingRuntimeDeps => {
            report.runtime_drift =
                crate::graph::runtime_can_start::recount_missing_runtime_deps(db).await? as i64;
        }
        Step::ReadRepairScope => {
            progress.covered_derivation_ids = crate::graph::can_start::can_start_scope(db).await?;
            report.repair_scope = progress.covered_derivation_ids.len() as i64;
        }
        Step::RepairFetchable => repair_fetchable_flag_for_next_slice(ctx, &mut progress).await?,
        Step::RecountWanted => {
            report.need_drift = crate::graph::can_start::recount_wanted(db).await? as i64;
        }
        Step::SettleSkipped => report.skipped_moves = settle_skipped_status(ctx).await?,
        Step::RecountBlockingDeps => recount_next_blocking_deps_slice(db, &mut progress).await?,
        Step::UnpromoteAndPromote => {
            report.unpromoted_startable = unpromote_and_promote(ctx).await?
        }
        Step::AdoptPendingClosures => adopt_pending_and_promote_slice(ctx, &mut progress).await?,
        Step::CountAlarms => count_alarms(db, report).await?,
    }

    Ok(progress)
}

async fn repair_fetchable_flag_for_next_slice(
    ctx: &DbContext,
    progress: &mut ConsistencyCheckRepairScopeChunkProgress,
) -> Result<(), DbErr> {
    let range = progress.take_next_slice_range(progress.covered_derivation_ids.len());
    let fetchable = crate::graph::can_start::repair_fetchable(
        &ctx.worker_db,
        &progress.covered_derivation_ids[range],
    )
    .await?;
    crate::status::emit_transition_effects(ctx, &fetchable.unqueued).await?;
    progress.report_so_far.counter_drift += fetchable.marked as i64;
    Ok(())
}

async fn settle_skipped_status(ctx: &DbContext) -> Result<i64, DbErr> {
    let settled = crate::graph::can_start::settle_skipped(&ctx.worker_db).await?;
    crate::status::emit_transition_effects(ctx, &settled).await?;
    Ok(settled.len() as i64)
}

async fn recount_next_blocking_deps_slice(
    db: &WorkerDb,
    progress: &mut ConsistencyCheckRepairScopeChunkProgress,
) -> Result<(), DbErr> {
    let range = progress.take_next_slice_range(progress.covered_derivation_ids.len());
    let blocking_deps = crate::graph::can_start::recount_blocking_deps_for_derivation_ids(
        db,
        &progress.covered_derivation_ids[range],
    )
    .await?;
    progress.report_so_far.counter_drift += blocking_deps as i64;
    Ok(())
}

async fn unpromote_and_promote(ctx: &DbContext) -> Result<i64, DbErr> {
    let moved =
        crate::graph::can_start::repair_all_shared_build_start_gate_states(&ctx.worker_db).await?;
    // Fan-out must follow the order the two statements ran in.
    // A row both moved would otherwise end on the board at the earlier status.
    crate::status::emit_transition_effects(ctx, &moved.unpromoted).await?;
    crate::status::emit_transition_effects(ctx, &moved.promoted).await?;
    Ok(moved.promoted.len() as i64)
}

async fn adopt_pending_and_promote_slice(
    ctx: &DbContext,
    progress: &mut ConsistencyCheckRepairScopeChunkProgress,
) -> Result<(), DbErr> {
    if !progress
        .derivation_ids_pending_forgotten_probe_rewalk
        .is_empty()
    {
        return rewalk_next_forgotten_probe_ids_then_promote(ctx, progress).await;
    }

    let db = &ctx.worker_db;
    let is_first_message_of_adopt_step = progress.pending_adopted_ids.is_none();
    let adopted = if is_first_message_of_adopt_step
        && crate::graph::reachability::pending_orphan_frontier(db).await?
    {
        crate::graph::reachability::adopt_pending_closures(db).await?
    } else {
        Adopted::default()
    };
    if is_first_message_of_adopt_step {
        progress.report_so_far.adopted = adopted.pairs.len() as i64;
        progress.pending_adopted_ids = Some(adopted.derivations());
    }

    let adopted_derivation_id_count = progress.pending_adopted_ids.as_ref().map_or(0, Vec::len);
    let range = progress.take_next_slice_range(adopted_derivation_id_count);
    if range.is_empty() {
        return Ok(());
    }
    let slice = &progress.pending_adopted_ids.as_deref().unwrap_or_default()[range];
    let need_update = crate::graph::can_start::update_and_settle_need_with_advisory_key_limit(
        db,
        slice,
        SWEEP_MAX_LOCK_KEYS,
    )
    .await?;
    let mut queued = need_update.changes;
    if need_update.deferred_rewalk_derivations.is_empty() {
        queued.extend(crate::graph::can_start::promote(db, slice).await?);
    } else {
        let promotion_waitlist = &mut progress.derivation_ids_waiting_for_promotion_after_rewalks;
        promotion_waitlist
            .extend(need_update.wanted_derivation_ids_to_promote_after_deferred_rewalks);
        promotion_waitlist.extend_from_slice(slice);
        promotion_waitlist.sort_unstable();
        promotion_waitlist.dedup();
        progress.derivation_ids_pending_forgotten_probe_rewalk =
            need_update.deferred_rewalk_derivations;
    }
    crate::task_board::dep_counts::bump_graph_version(db, &adopted.evaluations()).await?;
    crate::status::emit_transition_effects(ctx, &queued).await
}

async fn rewalk_next_forgotten_probe_ids_then_promote(
    ctx: &DbContext,
    progress: &mut ConsistencyCheckRepairScopeChunkProgress,
) -> Result<(), DbErr> {
    let pending_derivation_ids = &mut progress.derivation_ids_pending_forgotten_probe_rewalk;
    let slice: Vec<DerivationId> = pending_derivation_ids
        .drain(..pending_derivation_ids.len().min(SWEEP_MAX_LOCK_KEYS))
        .collect();
    let mut changes =
        crate::graph::can_start::recheck_forgotten_need(&ctx.worker_db, &slice).await?;
    if pending_derivation_ids.is_empty() {
        let promotion_waitlist =
            std::mem::take(&mut progress.derivation_ids_waiting_for_promotion_after_rewalks);
        for chunk in promotion_waitlist.chunks(crate::IN_CHUNK_SIZE) {
            changes.extend(crate::graph::can_start::promote(&ctx.worker_db, chunk).await?);
        }
    }
    crate::status::emit_transition_effects(ctx, &changes).await
}

async fn count_alarms(db: &WorkerDb, report: &mut ConsistencyReport) -> Result<(), DbErr> {
    report.unbacked_trusted_outputs = count(db, UNBACKED_TRUSTED_OUTPUT_COUNT.stmt()).await?;
    report.eval_counter_drift =
        crate::evaluations::counters::recount_eval_shared_build_counters(db).await? as i64;
    report.wedged_building_evals = count(db, WEDGED_BUILDING_EVALS.stmt()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{mock_db, statements};
    use sea_orm::{MockExecResult, Value};
    use std::collections::BTreeMap;

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    fn scripted(hole: bool) -> sea_orm::DatabaseConnection {
        let n = || vec![BTreeMap::from([("n".to_owned(), Value::BigInt(Some(0)))])];
        let locked = || {
            vec![BTreeMap::from([(
                "?column?".to_owned(),
                Value::Int(Some(1)),
            )])]
        };
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let drifted: Vec<BTreeMap<String, Value>> = (0..4)
            .map(|_| {
                BTreeMap::from([
                    ("derivation".to_owned(), Value::from(uuid::Uuid::now_v7())),
                    ("wanted".to_owned(), Value::from(true)),
                ])
            })
            .collect();
        let drift = || {
            vec![BTreeMap::from([(
                "?column?".to_owned(),
                Value::Int(Some(1)),
            )])]
        };
        let mut db = mock_db()
            .append_exec_results([exec(0), exec(7), exec(0), exec(6)])
            .append_query_results([drift(), drift()])
            .append_query_results([vec![BTreeMap::from([(
                "derivation".to_owned(),
                Value::from(uuid::Uuid::now_v7()),
            )])]])
            .append_exec_results([exec(0)])
            .append_query_results([
                locked(),
                vec![BTreeMap::from([(
                    "derivation".to_owned(),
                    Value::from(uuid::Uuid::now_v7()),
                )])],
                empty.clone(),
                empty.clone(),
            ])
            .append_query_results([drifted])
            .append_query_results([empty.clone(), empty.clone()])
            .append_exec_results([exec(5)])
            .append_query_results([locked(), empty.clone(), empty.clone()]);
        db = if hole {
            db.append_query_results([vec![BTreeMap::from([(
                "?column?".to_owned(),
                Value::Int(Some(1)),
            )])]])
            .append_exec_results([exec(0)])
            .append_query_results([vec![BTreeMap::from([
                ("evaluation".to_owned(), Value::from(uuid::Uuid::now_v7())),
                ("derivation".to_owned(), Value::from(uuid::Uuid::now_v7())),
            ])]])
            .append_exec_results([exec(0), exec(0)])
            .append_query_results([empty.clone(), empty.clone()])
            .append_exec_results([exec(1)])
        } else {
            db.append_query_results([empty.clone()])
        };

        db.append_query_results([n()])
            .append_query_results([vec![BTreeMap::from([(
                "id".to_owned(),
                Value::from(uuid::Uuid::now_v7()),
            )])]])
            .append_query_results([vec![
                BTreeMap::from([(
                    "drifted".to_owned(),
                    Value::from(true)
                )]);
                2
            ]])
            .append_query_results([n()])
            .append_exec_results([exec(0)])
            .into_connection()
    }

    #[tokio::test]
    async fn the_report_repairs_both_counters_before_it_counts() {
        let (ctx, pool) = crate::test_ctx::ctx(scripted(false)).await;
        let report = graph_consistency_report(&ctx).await.unwrap();
        drop(ctx);

        assert_eq!(
            report.counter_drift, 6,
            "both can-start recounts are reported, not one of them"
        );
        assert_eq!(
            report.repair_scope, 1,
            "the can-start repair's scope is measured too"
        );
        assert_eq!(
            report.need_drift, 4,
            "every shared build the recount rewrote is reported"
        );
        assert_eq!(
            report.walk_drift, 7,
            "the walk recount comes before the need recount"
        );
        assert_eq!(
            report.runtime_drift, 6,
            "the complete-closure recount comes before the can-start repair that reads it"
        );
        assert_eq!(report.adopted, 0);
        assert_eq!(
            report.eval_counter_drift, 2,
            "the evaluation counters are recounted and their drift reported"
        );

        let log = statements(pool.into_transaction_log());
        assert!(
            log[0].contains("SET LOCAL work_mem")
                && log[1].contains("FROM marked_incomplete i FULL JOIN counts c")
                && log[2].contains("SET unwalked_inputs = coalesce(c.n, 0)"),
            "the walk recount runs under its own raise, first of all: {log:?}"
        );
        assert!(
            log[3].contains("SET LOCAL work_mem")
                && log[4].contains("FROM marked_incomplete i FULL JOIN counts c")
                && log[5].contains("SET missing_runtime_deps = coalesce(c.n, 0)"),
            "the complete closure is recounted before anything that reads it: {log:?}"
        );
        assert!(
            log[6].contains("SELECT q.derivation FROM derivation_build q")
                && log[6].contains("q.fetchable AND q.missing_runtime_deps > 0"),
            "the repair scope is read once, contradicting flags included: {log:?}"
        );
        assert!(
            log[7].contains("FOR NO KEY UPDATE")
                && log[8].contains("SET fetchable = true")
                && log[9].contains("blocking_deps - c.n")
                && log[10].contains("SET fetchable = false"),
            "the flag is repaired under its own ordered lock, before the need walk \
             that stops at a fetchable shared build: {log:?}"
        );
        assert!(
            log[11].contains("SET LOCAL work_mem") && log[12].contains("SET wanted ="),
            "the table-wide need walk takes place under its own raise, on a repaired flag: {log:?}"
        );
        assert!(
            log[13].contains("db.status IN (5, 10) AND db.wanted")
                && log[14].contains("db.status = 0 AND NOT db.wanted"),
            "both Skipped directions read the need this pass corrected: {log:?}"
        );
        assert!(
            log[15].contains("FOR NO KEY UPDATE") && log[16].contains("SET blocking_deps"),
            "the counter recount follows the need recount, in a second locked pass \
             over the same scope: {log:?}"
        );
        assert!(
            log[17].contains("SET status = 0") && log[18].contains("SET status = 1"),
            "the queue is settled against the repaired counters and the corrected need: {log:?}"
        );
        assert!(
            log[19].contains(
                "NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) LIMIT 1"
            ),
            "the naming backstop asks before it walks: {log:?}"
        );
        assert!(
            log[20].contains("SELECT DISTINCT o.hash"),
            "the unbacked alarm follows the repairs: {log:?}"
        );
        assert!(
            log[21].contains("SELECT id FROM evaluation WHERE status IN")
                && log[22].contains("pg_advisory_xact_lock")
                && log[23].contains("DELETE FROM evaluation_shared_build_delta"),
            "the evaluation counters are recounted under the fold's lock: {log:?}"
        );
        assert!(
            log[24].contains("active_shared_builds = 0"),
            "the wedged alarm reads the counters the recount just corrected: {log:?}"
        );
        assert_eq!(log.len(), 25, "{log:?}");
    }

    #[tokio::test]
    async fn the_sweep_adopts_when_a_live_evaluation_reaches_an_unnamed_pending_shared_build() {
        let (ctx, pool) = crate::test_ctx::ctx(scripted(true)).await;
        let report = graph_consistency_report(&ctx).await.unwrap();
        drop(ctx);

        assert_eq!(report.adopted, 1);
        let log = statements(pool.into_transaction_log());
        assert!(
            log[19].contains("LIMIT 1")
                && log[20].contains("SET LOCAL work_mem")
                && log[21].contains("INSERT INTO build_job"),
            "the probe guards the walk that names: {log:?}"
        );
        assert!(
            log[22].contains("SET LOCAL work_mem")
                && log[23].contains("ORDER BY derivation FOR NO KEY UPDATE")
                && log[24].contains("ON d.derivation = r.derivation ORDER BY r.derivation"),
            "a name gives the closure below it a need, in this pass and not the next: {log:?}"
        );
        assert!(
            log[25].contains("SET status = 1")
                && log[26].contains("graph_version = e.graph_version + 1"),
            "then queue what was named and bump: {log:?}"
        );
        assert!(
            log[27].contains("SELECT DISTINCT o.hash")
                && log[31].contains("active_shared_builds = 0"),
            "the alarms still come last: {log:?}"
        );
    }

    #[test]
    fn total_sums_every_dimension() {
        let r = ConsistencyReport {
            counter_drift: 1,
            walk_drift: 9,
            runtime_drift: 10,
            need_drift: 8,
            skipped_moves: 11,
            unpromoted_startable: 3,
            unbacked_trusted_outputs: 4,
            wedged_building_evals: 5,
            eval_counter_drift: 6,
            repair_scope: 2000,
            adopted: 2,
        };
        assert_eq!(r.total(), 59);
        assert_eq!(ConsistencyReport::default().total(), 0);
    }

    #[test]
    fn chunked_check_advances_slice_then_step_on_cursor_wrap() {
        use BuildGraphConsistencyCheckStep as Step;

        let bound = SWEEP_LOCK_CHUNK * SWEEP_CHUNKS_PER_TRANSACTION;
        let mut progress = ConsistencyCheckRepairScopeChunkProgress::default();

        assert_eq!(progress.take_next_slice_range(bound + 1), 0..bound);
        assert_eq!(
            progress.next_step_after_finished_step(Step::AdoptPendingClosures),
            Some(Step::AdoptPendingClosures)
        );
        assert_eq!(progress.take_next_slice_range(bound + 1), bound..bound + 1);
        assert_eq!(
            progress.next_step_after_finished_step(Step::AdoptPendingClosures),
            Some(Step::CountAlarms)
        );
        assert_eq!(
            progress.next_step_after_finished_step(Step::RecountWanted),
            Some(Step::SettleSkipped)
        );
    }
}
