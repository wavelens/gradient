/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The job key prefixes are living next to the SQL rebuilding them.
//! A drifting copy could silently open a dispatch gate.

use gradient_entity::build::BuildStatus;
use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, DispatchedJobKind, DispatchedJobOutcome, Entity as EDispatchedJob,
    Model as MDispatchedJob,
};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::ids::{
    DerivationBuildId, DispatchedJobId, EvaluationId, ExecJobId, ProjectId,
};
use gradient_types::{CDerivationBuild, CDerivationOutput, CEvaluation, CExecJob};
use sea_orm::sea_query::{Expr, InsertStatement, OnConflict, Query, SelectStatement, Value};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, ExprTrait, IntoActiveModel,
    Iterable, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use std::collections::HashMap;

pub const EVAL_KEY_PREFIX: &str = "eval:";
pub const BUILD_KEY_PREFIX: &str = "build:";
pub const EXEC_KEY_PREFIX: &str = "exec:";
pub const CONTINUE_KEY_PREFIX: &str = "continue:";

pub fn shared_build_of_key(job_id: &str) -> Option<DerivationBuildId> {
    job_id.strip_prefix(BUILD_KEY_PREFIX)?.parse().ok()
}

pub fn eval_of_key(job_id: &str) -> Option<EvaluationId> {
    job_id.strip_prefix(EVAL_KEY_PREFIX)?.parse().ok()
}

pub fn exec_job_of_key(job_id: &str) -> Option<ExecJobId> {
    job_id.strip_prefix(EXEC_KEY_PREFIX)?.parse().ok()
}

pub fn exec_job_of_continuation_key(job_id: &str) -> Option<ExecJobId> {
    job_id.strip_prefix(CONTINUE_KEY_PREFIX)?.parse().ok()
}

pub fn continuation_job_key_sql(id_expr: &str) -> String {
    format!("'{CONTINUE_KEY_PREFIX}' || {id_expr}::text")
}

pub fn exec_job_key_sql(id_expr: &str) -> String {
    format!("'{EXEC_KEY_PREFIX}' || {id_expr}::text")
}

pub fn eval_job_key_sql(id_expr: &str) -> String {
    format!("'{EVAL_KEY_PREFIX}' || {id_expr}::text")
}

pub fn build_job_key_sql(id_expr: &str) -> String {
    format!("'{BUILD_KEY_PREFIX}' || {id_expr}::text")
}

pub fn no_open_assignment_predicate(job_key_sql: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM dispatched_job dj WHERE dj.job_id = {job_key_sql} \
         AND dj.finished_at IS NULL)"
    )
}

#[derive(Debug, Clone)]
pub enum ClaimGate {
    Build {
        shared_build: DerivationBuildId,
        output_names_to_substitute: Option<Vec<String>>,
    },
    Eval {
        evaluation: EvaluationId,
    },
    Exec {
        exec_job: ExecJobId,
    },
}

impl ClaimGate {
    fn holds(self) -> Expr {
        let subject = match self {
            ClaimGate::Build {
                shared_build,
                output_names_to_substitute,
            } => {
                let mut build = Query::select()
                    .expr(Expr::val(1))
                    .from(gradient_entity::derivation_build::Entity)
                    .and_where(CDerivationBuild::Id.eq(shared_build))
                    .and_where(CDerivationBuild::Status.eq(BuildStatus::Queued))
                    .and_where(
                        CDerivationBuild::CacheAvailable.eq(output_names_to_substitute.is_some()),
                    )
                    .to_owned();
                if let Some(names) = output_names_to_substitute {
                    build.and_where(
                        Expr::exists(
                            select_needed_outputs_missing_from_substitute_job_output_names(names),
                        )
                        .not(),
                    );
                }
                build
            }
            ClaimGate::Eval { evaluation } => Query::select()
                .expr(Expr::val(1))
                .from(gradient_entity::evaluation::Entity)
                .and_where(CEvaluation::Id.eq(evaluation))
                .and_where(CEvaluation::Status.is_not_in([
                    EvaluationStatus::Completed,
                    EvaluationStatus::Failed,
                    EvaluationStatus::Aborted,
                ]))
                .to_owned(),
            ClaimGate::Exec { exec_job } => Query::select()
                .expr(Expr::val(1))
                .from(gradient_entity::exec_job::Entity)
                .and_where(CExecJob::Id.eq(exec_job))
                .and_where(CExecJob::Status.eq(ExecJobStatus::Queued))
                .to_owned(),
        };

        Expr::exists(subject)
    }
}

fn select_needed_outputs_missing_from_substitute_job_output_names(
    names: Vec<String>,
) -> SelectStatement {
    use gradient_entity::derivation_output::Entity as EDerivationOutput;

    Query::select()
        .expr(Expr::val(1))
        .from(EDerivationOutput)
        .and_where(
            Expr::col((EDerivationOutput, CDerivationOutput::Derivation)).equals((
                gradient_entity::derivation_build::Entity,
                CDerivationBuild::Derivation,
            )),
        )
        .and_where(Expr::cust(
            "\"derivation_output\".\"name\" <> ALL(\"derivation_build\".\"unneeded_outputs\")",
        ))
        .and_where(Expr::col((EDerivationOutput, CDerivationOutput::Name)).is_not_in(names))
        .to_owned()
}

pub async fn claim_assignment<C: ConnectionTrait>(
    db: &C,
    row: MDispatchedJob,
    gate: ClaimGate,
) -> Result<bool, DbErr> {
    let claimed = db.execute(&claim_statement(row, gate)?).await?;

    Ok(claimed.rows_affected() == 1)
}

pub(crate) fn claim_statement(
    row: MDispatchedJob,
    gate: ClaimGate,
) -> Result<InsertStatement, DbErr> {
    let row = row.into_active_model();
    let (columns, values): (Vec<CDispatchedJob>, Vec<Expr>) = CDispatchedJob::iter()
        .filter_map(|c| row.get(c).into_value().map(|v| (c, Expr::val(v))))
        .unzip();

    let gated = Query::select()
        .exprs(values)
        .and_where(gate.holds())
        .to_owned();
    let mut insert = Query::insert();
    insert
        .into_table(EDispatchedJob)
        .columns(columns)
        .select_from(gated)
        .map_err(|e| DbErr::Custom(e.to_string()))?
        .on_conflict(
            OnConflict::column(CDispatchedJob::JobId)
                .target_and_where(Expr::col(CDispatchedJob::FinishedAt).is_null())
                .do_nothing()
                .to_owned(),
        );

    Ok(insert)
}

pub async fn latest_eval_jobs<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<HashMap<EvaluationId, DispatchedJobId>, DbErr> {
    let rows = crate::fetch_in_chunks(evaluations, |chunk| async move {
        EDispatchedJob::find()
            .select_only()
            .column(CDispatchedJob::EvaluationId)
            .column(CDispatchedJob::Id)
            .filter(CDispatchedJob::Kind.eq(DispatchedJobKind::Eval))
            .filter(CDispatchedJob::JobId.starts_with(EVAL_KEY_PREFIX))
            .filter(CDispatchedJob::EvaluationId.is_in(chunk))
            .order_by_asc(CDispatchedJob::DispatchedAt)
            .into_tuple::<(EvaluationId, DispatchedJobId)>()
            .all(db)
            .await
    })
    .await?;

    Ok(rows.into_iter().collect())
}

pub async fn find_newest_dispatched_job_assignment_for_job_keys<C: ConnectionTrait>(
    db: &C,
    job_keys: Vec<String>,
) -> Result<Option<(DispatchedJobId, EvaluationId, ProjectId)>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::EvaluationId)
        .column(CDispatchedJob::Project)
        .filter(CDispatchedJob::JobId.is_in(job_keys))
        .order_by_desc(CDispatchedJob::DispatchedAt)
        .into_tuple()
        .one(db)
        .await
}

pub async fn find_next_dispatched_job_id_for_job_key_after<C: ConnectionTrait>(
    db: &C,
    job_key: &str,
    after: chrono::NaiveDateTime,
) -> Result<Option<DispatchedJobId>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .filter(CDispatchedJob::JobId.eq(job_key))
        .filter(CDispatchedJob::DispatchedAt.gt(after))
        .order_by_asc(CDispatchedJob::DispatchedAt)
        .into_tuple::<DispatchedJobId>()
        .one(db)
        .await
}

pub async fn eval_attempts<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<u64, DbErr> {
    EDispatchedJob::find()
        .filter(CDispatchedJob::Kind.eq(DispatchedJobKind::Eval))
        .filter(CDispatchedJob::JobId.starts_with(EVAL_KEY_PREFIX))
        .filter(CDispatchedJob::EvaluationId.eq(evaluation))
        .filter(
            CDispatchedJob::Outcome
                .is_null()
                .or(CDispatchedJob::Outcome.ne(DispatchedJobOutcome::Abandoned)),
        )
        .count(db)
        .await
}

pub async fn abandon_open_assignments_for_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    before: chrono::NaiveDateTime,
) -> Result<u64, DbErr> {
    abandon_open(
        db,
        Some(
            CDispatchedJob::WorkerId
                .eq(worker_id)
                .and(CDispatchedJob::DispatchedAt.lt(before)),
        ),
    )
    .await
}

const RECOVERY_CLOSE_CHUNK: u64 = 10_000;

/// The statement is chunked at [`RECOVERY_CLOSE_CHUNK`] rows.
/// One statement over a six-figure backlog would hold WAL and row locks before the
/// listener is bound.
pub async fn abandon_open_cluster_assignments<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let chunk = format!(
        "\"dispatched_job\".\"id\" IN (SELECT id FROM dispatched_job \
         WHERE finished_at IS NULL AND (cluster_attempt IS NOT NULL OR job_id IS NULL) \
         LIMIT {RECOVERY_CLOSE_CHUNK})"
    );

    let mut closed = 0;
    loop {
        let rows = abandon_open(db, Some(Expr::cust(chunk.clone()))).await?;
        closed += rows;
        if rows < RECOVERY_CLOSE_CHUNK {
            return Ok(closed);
        }
    }
}

pub async fn abandon_open_assignments_for_jobs<C: ConnectionTrait>(
    db: &C,
    job_keys: &[String],
) -> Result<u64, DbErr> {
    if job_keys.is_empty() {
        return Ok(0);
    }

    abandon_open(db, Some(CDispatchedJob::JobId.is_in(job_keys.to_vec()))).await
}

pub async fn abandon_open_assignment<C: ConnectionTrait>(
    db: &C,
    assignment_id: DispatchedJobId,
) -> Result<u64, DbErr> {
    abandon_open(db, Some(CDispatchedJob::Id.eq(assignment_id))).await
}

pub async fn abandon_open_assignments<C: ConnectionTrait>(
    db: &C,
    assignments: &[DispatchedJobId],
) -> Result<u64, DbErr> {
    if assignments.is_empty() {
        return Ok(0);
    }

    let ids: Vec<uuid::Uuid> = assignments.iter().map(|d| d.into_inner()).collect();

    abandon_open(
        db,
        Some(Expr::cust_with_values(
            r#""dispatched_job"."id" = ANY($1)"#,
            [Value::from(ids)],
        )),
    )
    .await
}

pub async fn open_assignment_keys<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(DispatchedJobId, String, String)>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
        .column(CDispatchedJob::WorkerId)
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::JobId.is_not_null())
        .into_tuple()
        .all(db)
        .await
}

pub async fn open_single_assignments_of_disconnected_workers<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(DispatchedJobId, String, String)>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
        .column(CDispatchedJob::WorkerId)
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::ClusterAttempt.is_null())
        .filter(CDispatchedJob::JobId.is_not_null())
        .filter(Expr::cust(
            "NOT EXISTS (SELECT 1 FROM worker_connection \
             WHERE worker_connection.worker_id = \"dispatched_job\".\"worker_id\" \
             AND worker_connection.disconnected_at IS NULL)",
        ))
        .into_tuple()
        .all(db)
        .await
}

pub async fn keys_with_open_assignment<C: ConnectionTrait>(
    db: &C,
    job_keys: &[String],
) -> Result<Vec<String>, DbErr> {
    if job_keys.is_empty() {
        return Ok(Vec::new());
    }

    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::JobId)
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::JobId.is_in(job_keys.to_vec()))
        .distinct()
        .into_tuple()
        .all(db)
        .await
}

pub async fn open_single_assignments_of_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<Vec<(DispatchedJobId, String)>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
        .filter(CDispatchedJob::WorkerId.eq(worker_id))
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::ClusterAttempt.is_null())
        .filter(CDispatchedJob::JobId.is_not_null())
        .into_tuple()
        .all(db)
        .await
}

pub async fn assignment_rows<C: ConnectionTrait>(
    db: &C,
    ids: &[DispatchedJobId],
) -> Result<Vec<MDispatchedJob>, DbErr> {
    crate::fetch_in_chunks(ids, |chunk| async move {
        EDispatchedJob::find()
            .filter(CDispatchedJob::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await
}

pub async fn find_worker_id_and_project_by_dispatched_job_id<C: ConnectionTrait>(
    db: &C,
    dispatched_job: DispatchedJobId,
) -> Result<Option<(String, ProjectId)>, DbErr> {
    EDispatchedJob::find_by_id(dispatched_job)
        .select_only()
        .column(CDispatchedJob::WorkerId)
        .column(CDispatchedJob::Project)
        .into_tuple::<(String, ProjectId)>()
        .one(db)
        .await
}

pub(crate) async fn abandon_open<C: ConnectionTrait>(
    db: &C,
    scope: Option<Expr>,
) -> Result<u64, DbErr> {
    let mut update = EDispatchedJob::update_many()
        .col_expr(
            CDispatchedJob::FinishedAt,
            Expr::value(gradient_types::now()),
        )
        .col_expr(
            CDispatchedJob::Outcome,
            Expr::value(i16::from(DispatchedJobOutcome::Abandoned)),
        );
    if let Some(scope) = scope {
        update = update.filter(scope);
    }

    let res = update
        .filter(CDispatchedJob::FinishedAt.is_null())
        .exec(db)
        .await?;

    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build::BuildStatus;
    use gradient_entity::cluster_job::ClusterJobStatus;
    use gradient_entity::exec_job;
    use gradient_entity::ids::{ClusterAttemptId, ClusterJobId};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::shape::{build_assignment, build_job_key, eval_assignment, shape};

    #[test]
    fn an_exec_key_names_its_job() {
        let id = gradient_entity::ids::ExecJobId::now_v7();
        assert_eq!(exec_job_of_key(&format!("{EXEC_KEY_PREFIX}{id}")), Some(id));
        assert_eq!(exec_job_of_key(&format!("{BUILD_KEY_PREFIX}{id}")), None);
    }

    #[test]
    fn a_continuation_key_names_its_job() {
        let job = ExecJobId::now_v7();
        assert_eq!(
            exec_job_of_continuation_key(&format!("{CONTINUE_KEY_PREFIX}{job}")),
            Some(job)
        );
        assert_eq!(
            exec_job_of_continuation_key(&format!("{EVAL_KEY_PREFIX}{}", EvaluationId::now_v7())),
            None
        );
    }

    #[test]
    fn only_a_build_key_names_a_shared_build() {
        let shared_build = DerivationBuildId::now_v7();
        assert_eq!(
            shared_build_of_key(&format!("{BUILD_KEY_PREFIX}{shared_build}")),
            Some(shared_build)
        );
        assert_eq!(
            shared_build_of_key(&format!("{EVAL_KEY_PREFIX}{}", EvaluationId::now_v7())),
            None
        );
        assert_eq!(shared_build_of_key("build:not-a-uuid"), None);
    }

    #[test]
    fn only_an_eval_key_names_an_evaluation() {
        let evaluation = EvaluationId::now_v7();
        assert_eq!(
            eval_of_key(&format!("{EVAL_KEY_PREFIX}{evaluation}")),
            Some(evaluation)
        );
        assert_eq!(
            eval_of_key(&format!(
                "{BUILD_KEY_PREFIX}{}",
                DerivationBuildId::now_v7()
            )),
            None
        );
    }

    async fn claimed(
        db: &TestDatabase,
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
        output_names_to_substitute: Option<Vec<&str>>,
    ) -> bool {
        let row = build_assignment(
            evaluation,
            db.project_of(evaluation).await,
            db.task_of(evaluation).await,
            shared_build,
            gradient_types::now(),
        );
        let gate = ClaimGate::Build {
            shared_build,
            output_names_to_substitute: output_names_to_substitute
                .map(|names| names.into_iter().map(str::to_owned).collect()),
        };

        claim_assignment(&db.connection(), row, gate)
            .await
            .expect("the claim runs")
    }

    #[tokio::test]
    async fn claim_build_assignment_succeeds_only_for_queued_shared_build_in_matching_mode_without_open_assignment_for_job()
     {
        const QUEUED: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-queued.drv";
        const DONE: &str = "/nix/store/0c7kxbq0fdq6pnxpzhg5yvbbrylx4v3f-done.drv";
        const AVAILABLE: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-available.drv";
        const FULL: &str = "/nix/store/0c0f6hb8s2q2wbdrhl5ch9fwr8v2rq7x-full.drv";
        let evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(evaluation)
            .derivation(QUEUED)
            .build_status(BuildStatus::Queued)
            .derivation(DONE)
            .build_status(BuildStatus::Completed)
            .derivation(AVAILABLE)
            .output("11111111111111111111111111111111")
            .named_output("22222222222222222222222222222222", "dev")
            .unneeded_output("33333333333333333333333333333333", "doc")
            .build_status(BuildStatus::Queued)
            .cache_available()
            .derivation(FULL)
            .build_status(BuildStatus::Queued)
            .into_database()
            .await;
        let (queued, done, available, full) = (
            db.shared_build(QUEUED).await.id,
            db.shared_build(DONE).await.id,
            db.shared_build(AVAILABLE).await.id,
            db.shared_build(FULL).await.id,
        );

        let first = claimed(&db, evaluation, queued, None).await;
        let while_open = claimed(&db, evaluation, queued, None).await;
        abandon_open_assignments_for_jobs(&db.connection(), &[build_job_key(queued)])
            .await
            .expect("the assignment closes");
        let after_close = claimed(&db, evaluation, queued, None).await;
        let finished = claimed(&db, evaluation, done, None).await;
        let passthrough_of_available = claimed(&db, evaluation, available, None).await;
        let missing_needed_output = claimed(&db, evaluation, available, Some(vec!["out"])).await;
        let all_needed_outputs =
            claimed(&db, evaluation, available, Some(vec!["out", "dev"])).await;
        let substitute_of_unavailable = claimed(&db, evaluation, full, Some(vec!["out"])).await;

        let mut open: Vec<_> = db
            .rows::<EDispatchedJob>()
            .await
            .into_iter()
            .filter(|row| row.finished_at.is_none())
            .filter_map(|row| row.job_id)
            .collect();
        open.sort();
        db.finish().await;
        assert!(first && after_close && all_needed_outputs);
        assert!(
            !while_open
                && !finished
                && !passthrough_of_available
                && !missing_needed_output
                && !substitute_of_unavailable
        );
        let mut expected = vec![build_job_key(queued), build_job_key(available)];
        expected.sort();
        assert_eq!(open, expected);
    }

    async fn eval_row(db: &TestDatabase, evaluation: EvaluationId) -> MDispatchedJob {
        eval_assignment(
            evaluation,
            db.project_of(evaluation).await,
            db.task_of(evaluation).await,
            gradient_types::now(),
        )
    }

    async fn claim_eval(db: &TestDatabase, evaluation: EvaluationId) -> bool {
        claim_assignment(
            &db.connection(),
            eval_row(db, evaluation).await,
            ClaimGate::Eval { evaluation },
        )
        .await
        .expect("the claim runs")
    }

    async fn claim_exec(db: &TestDatabase, evaluation: EvaluationId, exec_job: ExecJobId) -> bool {
        let row = MDispatchedJob {
            kind: DispatchedJobKind::Exec,
            job_id: Some(format!("{EXEC_KEY_PREFIX}{exec_job}")),
            ..eval_row(db, evaluation).await
        };

        claim_assignment(&db.connection(), row, ClaimGate::Exec { exec_job })
            .await
            .expect("the claim runs")
    }

    #[tokio::test]
    async fn claim_wins_only_for_unfinished_evaluation_or_queued_exec_job() {
        let running = EvaluationId::now_v7();
        let ended: Vec<_> = [
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ]
        .into_iter()
        .map(|status| (EvaluationId::now_v7(), status))
        .collect();
        let mut seed = shape()
            .evaluation(running)
            .derivation("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-a.drv")
            .entry_point("pkg.a");
        for (evaluation, status) in &ended {
            seed = seed.evaluation(*evaluation).evaluation_status(*status);
        }
        let db = seed.into_database().await;
        let conn = db.connection();
        let point = db
            .entry_point(running, "pkg.a")
            .await
            .expect("the entry point");
        let (queued, finished) = (ExecJobId::now_v7(), ExecJobId::now_v7());
        for (id, status, node_index) in [
            (queued, ExecJobStatus::Queued, 0),
            (finished, ExecJobStatus::Completed, 1),
        ] {
            crate::rows::insert::<exec_job::Entity, _>(
                &conn,
                exec_job::Model {
                    id,
                    evaluation: running,
                    entry_point: point.id,
                    program_derivation: point.derivation,
                    requirement: serde_json::json!({}),
                    spec: serde_json::json!({}),
                    status,
                    created_at: gradient_types::now(),
                    node_index,
                    ..Default::default()
                },
            )
            .await
            .expect("the exec job is written");
        }
        let won_eval = claim_eval(&db, running).await;
        let mut lost_evals = Vec::new();
        for (evaluation, _) in &ended {
            lost_evals.push(claim_eval(&db, *evaluation).await);
        }
        let won_exec = claim_exec(&db, running, queued).await;
        let lost_exec = claim_exec(&db, running, finished).await;

        let stored = db.rows::<EDispatchedJob>().await.len();
        db.finish().await;
        assert!(won_eval && won_exec);
        assert_eq!(lost_evals, [false; 3]);
        assert!(!lost_exec);
        assert_eq!(stored, 2);
    }

    #[tokio::test]
    async fn latest_eval_job_per_evaluation_is_newest_eval_kind_assignment() {
        let (first, second, unrequested) = (
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
        );
        let ids: Vec<DispatchedJobId> = (0..6).map(|_| DispatchedJobId::now_v7()).collect();
        let [
            older,
            newer,
            newest_build,
            newest_continuation,
            only,
            elsewhere,
        ] = ids[..]
        else {
            unreachable!()
        };
        let at = gradient_types::now();
        let hours = |h: i64| at - chrono::Duration::hours(h);
        let db = shape()
            .evaluation(first)
            .eval_assignment()
            .assignment_option(|a| a.id = older)
            .dispatched_at(hours(3))
            .eval_assignment()
            .assignment_option(|a| {
                a.id = newer;
                a.job_id = Some(format!("{EVAL_KEY_PREFIX}second-attempt"));
            })
            .dispatched_at(hours(2))
            .eval_assignment()
            .assignment_option(|a| {
                a.id = newest_build;
                a.kind = DispatchedJobKind::Build;
                a.job_id = Some(format!("{BUILD_KEY_PREFIX}x"));
            })
            .dispatched_at(hours(1))
            .eval_assignment()
            .assignment_option(|a| {
                a.id = newest_continuation;
                a.job_id = Some(format!("{CONTINUE_KEY_PREFIX}x"));
            })
            .dispatched_at(hours(1))
            .evaluation(second)
            .eval_assignment()
            .assignment_option(|a| a.id = only)
            .dispatched_at(hours(4))
            .evaluation(unrequested)
            .eval_assignment()
            .assignment_option(|a| a.id = elsewhere)
            .into_database()
            .await;

        let latest = latest_eval_jobs(&db.connection(), &[first, second])
            .await
            .expect("the latest jobs read");

        db.finish().await;
        assert_eq!(latest, HashMap::from([(first, newer), (second, only)]));
    }

    #[tokio::test]
    async fn abandon_open_assignments_by_worker_cutoff_job_keys_single_id_and_id_list_closes_only_matching_open_rows_as_abandoned()
     {
        let now = gradient_types::now();
        let cutoff = now - chrono::Duration::hours(1);
        let long_ago = now - chrono::Duration::hours(2);
        let ids: Vec<DispatchedJobId> = (0..10).map(|_| DispatchedJobId::now_v7()).collect();
        let [
            before,
            after,
            other_worker,
            finished,
            by_build_key,
            by_eval_key,
            kept,
            single,
            listed_a,
            listed_b,
        ] = ids[..]
        else {
            unreachable!()
        };
        let specs = [
            (before, "w1", long_ago, "k1"),
            (after, "w1", now, "k2"),
            (other_worker, "w2", long_ago, "k3"),
            (finished, "w1", long_ago, "k4"),
            (by_build_key, "w3", now, "build:k5"),
            (by_eval_key, "w3", now, "eval:k6"),
            (kept, "w3", now, "k7"),
            (single, "w3", now, "k8"),
            (listed_a, "w3", now, "k9"),
            (listed_b, "w3", now, "k10"),
        ];
        let mut seed = shape().evaluation(EvaluationId::now_v7());
        for (id, worker, dispatched_at, key) in specs {
            seed = seed.eval_assignment().assignment_option(move |a| {
                a.id = id;
                a.worker_id = worker.into();
                a.dispatched_at = dispatched_at;
                a.job_id = Some(key.into());
            });
            if id == finished {
                seed = seed.closed(DispatchedJobOutcome::Failed, long_ago);
            }
        }
        let db = seed.into_database().await;
        let conn = db.connection();

        let by_worker = abandon_open_assignments_for_worker(&conn, "w1", cutoff).await;
        let by_keys = abandon_open_assignments_for_jobs(
            &conn,
            &["build:k5".to_owned(), "eval:k6".to_owned(), "k4".to_owned()],
        )
        .await;
        let by_id = abandon_open_assignment(&conn, single).await;
        let by_list = abandon_open_assignments(&conn, &[listed_a, listed_b]).await;
        let by_empty_list = abandon_open_assignments(&conn, &[]).await;
        let by_empty_keys = abandon_open_assignments_for_jobs(&conn, &[]).await;

        let rows: HashMap<_, _> = db
            .rows::<EDispatchedJob>()
            .await
            .into_iter()
            .map(|row| (row.id, (row.finished_at.is_some(), row.outcome)))
            .collect();
        db.finish().await;
        assert_eq!(by_worker.expect("update"), 1);
        assert_eq!(by_keys.expect("update"), 2);
        assert_eq!(by_id.expect("update"), 1);
        assert_eq!(by_list.expect("update"), 2);
        assert_eq!(by_empty_list.expect("no-op"), 0);
        assert_eq!(by_empty_keys.expect("no-op"), 0);
        let abandoned = (true, Some(DispatchedJobOutcome::Abandoned));
        for id in [
            before,
            by_build_key,
            by_eval_key,
            single,
            listed_a,
            listed_b,
        ] {
            assert_eq!(rows[&id], abandoned);
        }
        for id in [after, other_worker, kept] {
            assert_eq!(rows[&id], (false, None));
        }
        assert_eq!(rows[&finished], (true, Some(DispatchedJobOutcome::Failed)));
    }

    #[tokio::test]
    async fn startup_abandon_open_cluster_assignments_closes_only_cluster_or_keyless_rows_in_chunks_over_large_backlog()
     {
        let now = gradient_types::now();
        let (cluster, attempt) = (ClusterJobId::now_v7(), ClusterAttemptId::now_v7());
        let (keyless, single, finished_keyless) = (
            DispatchedJobId::now_v7(),
            DispatchedJobId::now_v7(),
            DispatchedJobId::now_v7(),
        );
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .cluster(cluster, true, 2, ClusterJobStatus::Running, 1, now)
            .evaluation_member_of(cluster, "server", true, None)
            .cluster_attempt(cluster, attempt, Some(now), None, None)
            .evaluation(EvaluationId::now_v7())
            .eval_assignment()
            .assignment_option(|a| {
                a.id = keyless;
                a.job_id = None;
            })
            .evaluation(EvaluationId::now_v7())
            .eval_assignment()
            .assignment_option(|a| a.id = single)
            .evaluation(EvaluationId::now_v7())
            .eval_assignment()
            .assignment_option(|a| {
                a.id = finished_keyless;
                a.job_id = None;
            })
            .closed(DispatchedJobOutcome::Completed, now)
            .into_database()
            .await;
        let conn = db.connection();
        let backlog = RECOVERY_CLOSE_CHUNK + 1;
        db.execute(&format!(
            "INSERT INTO dispatched_job \
             SELECT (jsonb_populate_record(NULL::dispatched_job, \
                       to_jsonb(t) || jsonb_build_object('id', uuidv7()))).* \
             FROM dispatched_job t, generate_series(1, {backlog}) WHERE t.id = '{}'",
            keyless.into_inner()
        ))
        .await;

        let closed = abandon_open_cluster_assignments(&conn)
            .await
            .expect("the backlog closes");

        let rows = db.rows::<EDispatchedJob>().await;
        db.finish().await;
        assert_eq!(closed, backlog + 2);
        let open: Vec<_> = rows.iter().filter(|r| r.finished_at.is_none()).collect();
        assert_eq!(open.iter().map(|r| r.id).collect::<Vec<_>>(), [single]);
        let finished = rows.iter().find(|r| r.id == finished_keyless).expect("row");
        assert_eq!(finished.outcome, Some(DispatchedJobOutcome::Completed));
        assert!(
            rows.iter()
                .filter(|r| r.outcome == Some(DispatchedJobOutcome::Abandoned))
                .count() as u64
                == backlog + 2
        );
    }

    #[tokio::test]
    async fn test_job_key_sql_fragments_parse_to_keys_and_no_open_assignment_filters_jobs_with_open_assignment()
     {
        const OPEN: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-open.drv";
        const CLOSED: &str = "/nix/store/0c7kxbq0fdq6pnxpzhg5yvbbrylx4v3f-closed.drv";
        const NONE: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-none.drv";
        let now = gradient_types::now();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(OPEN)
            .build_status(BuildStatus::Queued)
            .open_assignment()
            .derivation(CLOSED)
            .build_status(BuildStatus::Queued)
            .open_assignment()
            .closed(DispatchedJobOutcome::Completed, now)
            .derivation(NONE)
            .build_status(BuildStatus::Queued)
            .into_database()
            .await;
        let (build, evaluation, exec) = (
            DerivationBuildId::now_v7(),
            EvaluationId::now_v7(),
            ExecJobId::now_v7(),
        );
        let key = |fragment: String| {
            let db = &db;
            async move {
                db.texts(&format!("SELECT {fragment}"))
                    .await
                    .pop()
                    .expect("a key")
            }
        };
        let uuid = |id: uuid::Uuid| format!("'{id}'::uuid");

        let build_key = key(build_job_key_sql(&uuid(build.into_inner()))).await;
        let eval_key = key(eval_job_key_sql(&uuid(evaluation.into_inner()))).await;
        let exec_key = key(exec_job_key_sql(&uuid(exec.into_inner()))).await;
        let continuation_key = key(continuation_job_key_sql(&uuid(exec.into_inner()))).await;
        let without_open = db
            .texts(&format!(
                "SELECT b.id::text FROM derivation_build b WHERE {} ORDER BY 1",
                no_open_assignment_predicate(&build_job_key_sql("b.id"))
            ))
            .await;
        let (open, closed, none) = (
            db.shared_build(OPEN).await.id,
            db.shared_build(CLOSED).await.id,
            db.shared_build(NONE).await.id,
        );

        db.finish().await;
        assert_eq!(shared_build_of_key(&build_key), Some(build));
        assert_eq!(eval_of_key(&eval_key), Some(evaluation));
        assert_eq!(exec_job_of_key(&exec_key), Some(exec));
        assert_eq!(exec_job_of_continuation_key(&continuation_key), Some(exec));
        let mut expected = vec![closed.to_string(), none.to_string()];
        expected.sort();
        assert_eq!(without_open, expected);
        assert!(!without_open.contains(&open.to_string()));
    }
}
