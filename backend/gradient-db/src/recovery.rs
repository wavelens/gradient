/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, QueryFilter, Statement,
};

use gradient_types::*;

/// Evaluations the scheduler re-drives on its own after a restart, so recovery
/// must leave them alone: `Queued` is re-offered by the eval dispatcher and
/// `Waiting` (evaluated, builds queued for a free worker) by build reconcile.
/// Every other active status was running on a now-disconnected worker and is
/// genuinely lost.
fn eval_survives_restart(status: EvaluationStatus) -> bool {
    matches!(status, EvaluationStatus::Queued | EvaluationStatus::Waiting)
}

/// The active statuses this sweep aborts. Derived from `ACTIVE` rather than
/// listed, so a newly added active status is recovered by default instead of
/// silently surviving a restart it cannot survive.
fn lost_eval_statuses() -> Vec<EvaluationStatus> {
    EvaluationStatus::ACTIVE
        .into_iter()
        .filter(|s| !eval_survives_restart(*s))
        .collect()
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Open `dispatched_job` rows closed as `Abandoned`, each one a dispatch
    /// gate reopened without waiting for its worker to come back.
    pub dispatches_closed: u64,
    pub attempts_aborted: u64,
    pub builds_requeued: u64,
    /// How many of `builds_requeued` the gate pulled straight back to `Created`.
    pub builds_unpromoted: u64,
    pub builds_aborted: u64,
    pub evals_aborted: u64,
    pub tasks_forced: u64,
}

pub async fn recover_interrupted_work<C: ConnectionTrait>(
    conn: &C,
) -> Result<RecoveryReport, DbErr> {
    // 1. Nothing the previous process handed out is still out: close its
    // dispatch rows before the requeue below, or both dispatch selections
    // refuse the work they re-queue until each worker reconnects - which a
    // scaled-down or crashed one never does - or the 1800 s sweep runs.
    let mut report = RecoveryReport {
        dispatches_closed: crate::dispatch_record::abandon_all_open_dispatches(conn).await?,
        ..Default::default()
    };

    // 2. Abort orphaned running attempts.
    let now = now();
    let res = gradient_entity::build_attempt::Entity::update_many()
        .col_expr(
            gradient_entity::build_attempt::Column::Outcome,
            Expr::value(AttemptOutcome::Aborted),
        )
        .col_expr(
            gradient_entity::build_attempt::Column::BuildFinishedAt,
            Expr::value(now),
        )
        .filter(gradient_entity::build_attempt::Column::Outcome.eq(AttemptOutcome::Running))
        .exec(conn)
        .await?;
    report.attempts_aborted = res.rows_affected;

    // 3. Re-queue anchors that were mid-flight (Building back to Queued). The
    // requeue evaluates no gate and since #591 nothing downstream re-checks a
    // `Queued` row, so it settles what it wrote: a mid-flight anchor's dependencies
    // can have regressed while the server was down, and on the first start after
    // `m20260908_000000` every derivation is unwalked. `RETURNING` names exactly the
    // rows this statement moved, so the settle cannot miss one that arrived late.
    let requeued = crate::promotion::returned_derivations(
        conn.query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE derivation_build SET status = {queued}, \
                 updated_at = (now() AT TIME ZONE 'UTC') \
                 WHERE status = {building} RETURNING derivation",
                queued = crate::status_sql::build(BuildStatus::Queued),
                building = crate::status_sql::build(BuildStatus::Building),
            ),
        ))
        .await?,
    );
    report.builds_requeued = requeued.len() as u64;
    report.builds_unpromoted = crate::readiness::unpromote_ungated(conn, &requeued)
        .await?
        .len() as u64;
    crate::dep_counts::bump_graph_version_for_derivations(conn, &requeued).await?;

    // 4a. Collect the evals a restart lost, `Building` included: their anchors
    // and their terminal transition are this sweep's to finish.
    let inflight_evals = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(lost_eval_statuses()))
        .all(conn)
        .await?;

    // 4b. Abort those evaluations as a complete terminal transition. `finished_at`
    // belongs with the status: a reader that sees Aborted with no end time reads
    // it as still running, and retention keys off the column. The live path
    // (`update_evaluation_status`) also runs the reactor effects; startup has no
    // context for those, so the row is at least consistent on its own.
    let eval_ids: Vec<EvaluationId> = inflight_evals.iter().map(|e| e.id).collect();
    if !eval_ids.is_empty() {
        let res = EEvaluation::update_many()
            .col_expr(CEvaluation::Status, Expr::value(EvaluationStatus::Aborted))
            .col_expr(CEvaluation::UpdatedAt, Expr::value(now))
            .col_expr(CEvaluation::FinishedAt, Expr::value(now))
            .col_expr(
                CEvaluation::GraphVersion,
                Expr::col(CEvaluation::GraphVersion).add(1),
            )
            .filter(CEvaluation::Id.is_in(eval_ids.clone()))
            .exec(conn)
            .await?;
        report.evals_aborted = res.rows_affected;

        let subjects: Vec<uuid::Uuid> = eval_ids.iter().map(|e| e.into_inner()).collect();
        crate::status::record_phase_events(
            conn,
            crate::status::PhaseSubjectKind::Evaluation,
            &subjects,
            i32::from(EvaluationStatus::Aborted) as i16,
            now,
        )
        .await;
    }

    // 4c. Abort the anchors those evals drove. When the server dies mid-eval the
    // builder aborts the eval's builds, so reflect it: Created/Queued/Building
    // anchors referenced only by the now-aborted evals go to Aborted. Anchors a
    // still-live eval also needs are left running (shared-anchor safety). The
    // force-eval below re-drives them - `requeue_failed_anchors` resets
    // Aborted -> Created on the next evaluation.
    // The aborted anchors are shared: an evaluation that was already terminal
    // when the server died still shows them, and step 3b only bumped the lost
    // evaluations, so their histograms need the bump keyed on the derivations.
    if !eval_ids.is_empty() {
        let aborted = abort_anchors_for_evals(conn, &eval_ids).await?;
        report.builds_aborted = aborted.len() as u64;
        crate::dep_counts::bump_graph_version_for_derivations(conn, &aborted).await?;
    }

    // 4d. Force re-evaluation of the affected tasks.
    let task_ids: Vec<TaskId> = inflight_evals
        .into_iter()
        .filter_map(|e| e.task)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if !task_ids.is_empty() {
        let res = ETask::update_many()
            .col_expr(CTask::ForceEvaluation, Expr::value(true))
            .filter(CTask::Id.is_in(task_ids))
            .exec(conn)
            .await?;
        report.tasks_forced = res.rows_affected;
    }

    Ok(report)
}

/// Abort the non-terminal anchors (`Created`/`Queued`/`Building`) driven by
/// `eval_ids`, skipping any a still-live (non-terminal) evaluation also needs.
/// Mirrors the explicit-abort path (`status::abort`): a global build-once anchor
/// is only aborted when no surviving evaluation depends on it. Returns the
/// derivations it aborted, which name every evaluation whose histogram moved.
async fn abort_anchors_for_evals<C: ConnectionTrait>(
    conn: &C,
    eval_ids: &[EvaluationId],
) -> Result<Vec<DerivationId>, DbErr> {
    let ids: Vec<uuid::Uuid> = eval_ids.iter().map(|e| e.into_inner()).collect();
    let sql = format!(
        r#"
        UPDATE derivation_build db
        SET status = {aborted}, updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.status IN ({created}, {queued}, {building})
          AND EXISTS (
            SELECT 1 FROM build_job bj
            WHERE bj.derivation_build = db.id AND bj.evaluation = ANY($1))
          AND NOT EXISTS (
            SELECT 1 FROM build_job bj2
            JOIN evaluation e2 ON e2.id = bj2.evaluation
            WHERE bj2.derivation_build = db.id
              AND e2.status NOT IN ({completed}, {failed}, {eval_aborted}))
        RETURNING db.derivation
        "#,
        aborted = BuildStatus::Aborted as i32,
        created = BuildStatus::Created as i32,
        queued = BuildStatus::Queued as i32,
        building = BuildStatus::Building as i32,
        completed = EvaluationStatus::Completed as i32,
        failed = EvaluationStatus::Failed as i32,
        eval_aborted = EvaluationStatus::Aborted as i32,
    );

    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [ids.into()],
        ))
        .await?;

    Ok(crate::promotion::returned_derivations(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Building` is the status this sweep used to miss. Startup aborted such an
    /// evaluation before recovery looked for it, so `abort_anchors_for_evals`
    /// found nothing and every anchor it drove stayed Created/Queued forever.
    #[test]
    fn recovery_owns_every_active_status_a_restart_loses() {
        let lost = lost_eval_statuses();
        assert!(lost.contains(&EvaluationStatus::Building), "{lost:?}");
        assert!(lost.contains(&EvaluationStatus::Fetching), "{lost:?}");
        assert!(
            lost.contains(&EvaluationStatus::EvaluatingFlake),
            "{lost:?}"
        );
        assert!(
            lost.contains(&EvaluationStatus::EvaluatingDerivation),
            "{lost:?}"
        );
    }

    /// The scheduler re-drives these two itself; aborting them would cancel work
    /// that is still live.
    #[test]
    fn recovery_leaves_the_statuses_the_scheduler_re_drives() {
        let lost = lost_eval_statuses();
        assert!(!lost.contains(&EvaluationStatus::Queued), "{lost:?}");
        assert!(!lost.contains(&EvaluationStatus::Waiting), "{lost:?}");
    }

    /// Derived from `ACTIVE`, never listed: a newly added active status must be
    /// recovered by default rather than silently surviving a restart.
    #[test]
    fn the_lost_set_is_exactly_active_minus_the_survivors() {
        let lost = lost_eval_statuses();
        let expected: Vec<EvaluationStatus> = EvaluationStatus::ACTIVE
            .into_iter()
            .filter(|s| !matches!(s, EvaluationStatus::Queued | EvaluationStatus::Waiting))
            .collect();
        assert_eq!(lost, expected);
        assert_eq!(lost.len(), EvaluationStatus::ACTIVE.len() - 2);
    }

    use gradient_entity::evaluation::Model as MEval;
    use gradient_entity::ids::{CommitId, EvaluationId, TaskId};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn derivation_row(derivation: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([(
            "derivation".to_owned(),
            Value::from(derivation.into_inner()),
        )])
    }

    fn unpromoted_row(derivation: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            (
                "from_status".to_owned(),
                Value::from(crate::status_sql::build(BuildStatus::Queued)),
            ),
            (
                "to_status".to_owned(),
                Value::from(crate::status_sql::build(BuildStatus::Created)),
            ),
        ])
    }

    fn eval_row(status: EvaluationStatus, task: Option<TaskId>) -> MEval {
        MEval {
            id: EvaluationId::now_v7(),
            task,
            status,
            repository: "git+https://example.com/repo".into(),
            commit: CommitId::now_v7(),
            wildcard: "**".into(),
            created_at: now(),
            updated_at: now(),
            ..Default::default()
        }
    }

    /// The mid-flight requeue evaluates no gate, so the settle after it is part of
    /// the step and not an optimisation: an anchor whose dependencies regressed while
    /// the server was down must leave the queue again before the first dispatch pass
    /// reads it. It settles the requeue's own `RETURNING` rows, so `builds_requeued`
    /// and the settle's scope cannot disagree, and the count is reported separately.
    #[tokio::test]
    async fn all_operations_populate_report() {
        let task_id = TaskId::now_v7();
        let mid_flight = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. close the dispatch rows of the process that died
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 5,
            }])
            // 2. abort orphaned attempts
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 3,
            }])
            // 3. re-queue Building builds, naming the rows it moved
            .append_query_results([vec![
                derivation_row(mid_flight),
                derivation_row(DerivationId::now_v7()),
            ]])
            // 3. the settle pulls one of them straight back
            .append_query_results([vec![unpromoted_row(mid_flight)]])
            // 3. the requeue bumps the graph version of the evals it touched
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // 4a. SELECT pre-build inflight evals
            .append_query_results([vec![eval_row(EvaluationStatus::Fetching, Some(task_id))]])
            // 4b. abort those evals
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // 4c. abort their anchors, naming the derivations they moved
            .append_query_results([vec![
                derivation_row(DerivationId::now_v7()),
                derivation_row(DerivationId::now_v7()),
                derivation_row(DerivationId::now_v7()),
                derivation_row(DerivationId::now_v7()),
            ]])
            // 4c. bump the evaluations still showing those anchors
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            // 4d. force-eval their tasks
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.dispatches_closed, 5);
        assert_eq!(report.attempts_aborted, 3);
        assert_eq!(report.builds_requeued, 2);
        assert_eq!(report.builds_unpromoted, 1);
        assert_eq!(report.builds_aborted, 4);
        assert_eq!(report.evals_aborted, 1);
        assert_eq!(report.tasks_forced, 1);
    }

    /// Recovery asserts that nothing the previous process handed out is still
    /// out, so it owns the dispatch rows too: an anchor requeued while its row
    /// is still open is refused by `find_ready_anchors` until that worker
    /// reconnects, which a scaled-down or crashed one never does.
    #[tokio::test]
    async fn the_dispatch_rows_close_before_the_anchor_requeue() {
        let none = MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([none.clone(), none])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<MEval>::new()])
            .into_connection();

        recover_interrupted_work(&db).await.unwrap();

        let log = db.into_transaction_log();
        let sql: Vec<&str> = log
            .iter()
            .flat_map(|t| t.statements())
            .map(|s| s.sql.as_str())
            .collect();
        let close = sql
            .iter()
            .position(|s| s.starts_with("UPDATE \"dispatched_job\""))
            .expect("recovery closes every open dispatch row");
        let requeue = sql
            .iter()
            .position(|s| s.contains("UPDATE derivation_build SET status"))
            .expect("recovery requeues the mid-flight anchors");
        assert!(close < requeue, "{sql:?}");
    }

    #[tokio::test]
    async fn task_force_step_skipped_when_no_pre_build_evals() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. no dispatch rows were left open
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            // 2. abort orphaned attempts (none)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            // 3. nothing was mid-flight, so the settle issues no statement at all
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            // 4a. SELECT pre-build evals: empty, so steps 4b/4c/4d are skipped
            .append_query_results([Vec::<MEval>::new()])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.dispatches_closed, 0);
        assert_eq!(report.attempts_aborted, 0);
        assert_eq!(report.builds_requeued, 0);
        assert_eq!(report.builds_unpromoted, 0);
        assert_eq!(report.builds_aborted, 0);
        assert_eq!(report.evals_aborted, 0);
        assert_eq!(report.tasks_forced, 0);
    }
}
