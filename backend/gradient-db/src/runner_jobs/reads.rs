/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::runner_job::RunnerJobStatus;
use gradient_types::{
    CRunnerJob, CRunnerJobAttempt, CRunnerJobPhase, EEntryPoint, EEvaluation, ERunnerJob,
    ERunnerJobAttempt, ERunnerJobPhase, ETask, EvaluationId, MRunnerJob, MRunnerJobAttempt,
    ProjectId, RunnerJobAttemptId, RunnerJobId, TaskId,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    QueryResult, QuerySelect, Value,
};

#[derive(Debug, Clone)]
pub struct RunnerJobRow {
    pub job: MRunnerJob,
    pub entry_point: String,
    pub last_phase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct RunnerJobPhaseRow {
    pub attempt: RunnerJobAttemptId,
    pub name: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone)]
pub struct RunnerJobDetail {
    pub job: MRunnerJob,
    pub entry_point: String,
    pub task: Option<TaskId>,
    pub project: Option<ProjectId>,
    pub attempts: Vec<MRunnerJobAttempt>,
    pub phases: Vec<RunnerJobPhaseRow>,
}

crate::sql! {
    RUNNER_JOBS_OF_EVALUATION = "SELECT r.*, e.eval AS entry_point_name, \
         (SELECT p.name FROM runner_job_phase p WHERE p.runner_job = r.id \
              ORDER BY p.created_at DESC LIMIT 1) AS last_phase_name \
         FROM runner_job r JOIN entry_point e ON e.id = r.entry_point \
         WHERE r.evaluation = $1 ORDER BY e.eval",
        params = [EvaluationId];
}

pub async fn open_of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<RunnerJobId>, DbErr> {
    ERunnerJob::find()
        .select_only()
        .column(CRunnerJob::Id)
        .filter(CRunnerJob::Evaluation.eq(evaluation))
        .filter(CRunnerJob::Status.is_in(RunnerJobStatus::OPEN))
        .into_tuple()
        .all(db)
        .await
}

pub async fn of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<RunnerJobRow>, DbErr> {
    db.query_all_raw(RUNNER_JOBS_OF_EVALUATION.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?
        .iter()
        .map(job_row)
        .collect()
}

fn job_row(row: &QueryResult) -> Result<RunnerJobRow, DbErr> {
    Ok(RunnerJobRow {
        job: MRunnerJob::from_query_result(row, "")?,
        entry_point: row.try_get("", "entry_point_name")?,
        last_phase: row.try_get("", "last_phase_name")?,
    })
}

pub async fn one<C: ConnectionTrait>(
    db: &C,
    job: RunnerJobId,
) -> Result<Option<RunnerJobDetail>, DbErr> {
    let Some(job) = ERunnerJob::find_by_id(job).one(db).await? else {
        return Ok(None);
    };
    let entry_point = EEntryPoint::find_by_id(job.entry_point)
        .one(db)
        .await?
        .map(|e| e.eval)
        .unwrap_or_default();
    let task = EEvaluation::find_by_id(job.evaluation)
        .one(db)
        .await?
        .and_then(|e| e.task);
    let project = match task {
        Some(task) => ETask::find_by_id(task).one(db).await?.map(|t| t.project),
        None => None,
    };
    let attempts = ERunnerJobAttempt::find()
        .filter(CRunnerJobAttempt::RunnerJob.eq(job.id))
        .order_by_asc(CRunnerJobAttempt::Number)
        .all(db)
        .await?;
    let phases = ERunnerJobPhase::find()
        .select_only()
        .column(CRunnerJobPhase::Attempt)
        .column(CRunnerJobPhase::Name)
        .column(CRunnerJobPhase::CreatedAt)
        .filter(CRunnerJobPhase::RunnerJob.eq(job.id))
        .order_by_asc(CRunnerJobPhase::CreatedAt)
        .into_model::<RunnerJobPhaseRow>()
        .all(db)
        .await?;
    Ok(Some(RunnerJobDetail {
        job,
        entry_point,
        task,
        project,
        attempts,
        phases,
    }))
}
