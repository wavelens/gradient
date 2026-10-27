/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::{
    CExecJob, CExecJobAttempt, CExecJobPhase, EEntryPoint, EEvaluation, EExecJob, EExecJobAttempt,
    EExecJobPhase, ETask, EvaluationId, ExecJobAttemptId, ExecJobId, MExecJob, MExecJobAttempt,
    ProjectId, TaskId,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    QueryResult, QuerySelect, Value,
};

#[derive(Debug, Clone)]
pub struct ExecJobRow {
    pub job: MExecJob,
    pub entry_point: String,
    pub last_phase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct ExecJobPhaseRow {
    pub attempt: ExecJobAttemptId,
    pub name: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone)]
pub struct ExecJobDetail {
    pub job: MExecJob,
    pub entry_point: String,
    pub task: Option<TaskId>,
    pub project: Option<ProjectId>,
    pub attempts: Vec<MExecJobAttempt>,
    pub phases: Vec<ExecJobPhaseRow>,
}

crate::sql! {
    EXEC_JOBS_OF_EVALUATION = "SELECT r.*, e.eval AS entry_point_name, \
         (SELECT p.name FROM exec_job_phase p WHERE p.exec_job = r.id \
              ORDER BY p.created_at DESC, p.id DESC LIMIT 1) AS last_phase_name \
         FROM exec_job r JOIN entry_point e ON e.id = r.entry_point \
         WHERE r.evaluation = $1 ORDER BY e.eval",
        params = [EvaluationId];
}

pub async fn open_of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<ExecJobId>, DbErr> {
    EExecJob::find()
        .select_only()
        .column(CExecJob::Id)
        .filter(CExecJob::Evaluation.eq(evaluation))
        .filter(CExecJob::Status.is_in(ExecJobStatus::OPEN))
        .into_tuple()
        .all(db)
        .await
}

pub async fn of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<ExecJobRow>, DbErr> {
    db.query_all_raw(EXEC_JOBS_OF_EVALUATION.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?
        .iter()
        .map(job_row)
        .collect()
}

fn job_row(row: &QueryResult) -> Result<ExecJobRow, DbErr> {
    Ok(ExecJobRow {
        job: MExecJob::from_query_result(row, "")?,
        entry_point: row.try_get("", "entry_point_name")?,
        last_phase: row.try_get("", "last_phase_name")?,
    })
}

pub async fn one<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
) -> Result<Option<ExecJobDetail>, DbErr> {
    let Some(job) = EExecJob::find_by_id(job).one(db).await? else {
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
    let attempts = EExecJobAttempt::find()
        .filter(CExecJobAttempt::ExecJob.eq(job.id))
        .order_by_asc(CExecJobAttempt::Number)
        .all(db)
        .await?;
    let phases = EExecJobPhase::find()
        .select_only()
        .column(CExecJobPhase::Attempt)
        .column(CExecJobPhase::Name)
        .column(CExecJobPhase::CreatedAt)
        .filter(CExecJobPhase::ExecJob.eq(job.id))
        .order_by_asc(CExecJobPhase::CreatedAt)
        .order_by_asc(CExecJobPhase::Id)
        .into_model::<ExecJobPhaseRow>()
        .all(db)
        .await?;
    Ok(Some(ExecJobDetail {
        job,
        entry_point,
        task,
        project,
        attempts,
        phases,
    }))
}
