/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::nodes::{aggregate_status, nodes_of};
use super::tokens::JobTokenRow;
use chrono::NaiveDateTime;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::{
    CEvaluation, CExecJob, CExecJobAttempt, CExecJobPhase, EEntryPoint, EEvaluation, EExecJob,
    EExecJobAttempt, EExecJobPhase, ETask, EntryPointId, EvaluationId, ExecJobAttemptId, ExecJobId,
    MExecJob, MExecJobAttempt, ProjectId, TaskId,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, JoinType, QueryFilter,
    QueryOrder, QueryResult, QuerySelect, RelationTrait, Value,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ExecJobRow {
    pub job: MExecJob,
    pub status: ExecJobStatus,
    pub entry_point: String,
    pub last_phase: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecNodeDetail {
    pub job: MExecJob,
    pub attempts: Vec<MExecJobAttempt>,
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
    pub status: ExecJobStatus,
    pub entry_point: String,
    pub task: Option<TaskId>,
    pub project: Option<ProjectId>,
    pub attempts: Vec<MExecJobAttempt>,
    pub phases: Vec<ExecJobPhaseRow>,
    pub tokens: Vec<JobTokenRow>,
    pub started_evaluations: Vec<StartedEvaluation>,
    pub nodes: Vec<ExecNodeDetail>,
}

impl ExecJobDetail {
    pub fn node(&self, index: u32) -> Option<&ExecNodeDetail> {
        self.nodes
            .iter()
            .find(|node| u32::try_from(node.job.node_index).ok() == Some(index))
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct StartedEvaluation {
    pub evaluation: EvaluationId,
    pub task: String,
    pub status: EvaluationStatus,
    pub created_at: NaiveDateTime,
}

crate::sql! {
    EXEC_JOBS_OF_EVALUATION = "SELECT r.*, e.eval AS entry_point_name, \
         (SELECT p.name FROM exec_job_phase p WHERE p.exec_job = r.id \
              ORDER BY p.created_at DESC, p.id DESC LIMIT 1) AS last_phase_name \
         FROM exec_job r JOIN entry_point e ON e.id = r.entry_point \
         WHERE r.evaluation = $1 ORDER BY e.eval",
        params = [EvaluationId];
    EVALUATIONS_STARTED_BY_EXEC_JOB = "SELECT ev.id AS evaluation, t.name AS task, ev.status, \
         ev.created_at FROM evaluation ev JOIN task t ON t.id = ev.task \
         WHERE ev.parent_exec_job = $1 ORDER BY ev.created_at",
        params = [NewUuid];
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

pub async fn latest_running_of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<Option<ExecJobId>, DbErr> {
    EExecJob::find()
        .select_only()
        .column(CExecJob::Id)
        .join(
            JoinType::InnerJoin,
            gradient_entity::exec_job::Relation::Evaluation.def(),
        )
        .filter(CEvaluation::Task.eq(task))
        .filter(CExecJob::Status.eq(ExecJobStatus::Running))
        .order_by_desc(CExecJob::StartedAt)
        .limit(1)
        .into_tuple()
        .one(db)
        .await
}

pub async fn of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<ExecJobRow>, DbErr> {
    let rows = db
        .query_all_raw(EXEC_JOBS_OF_EVALUATION.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?
        .iter()
        .map(job_row)
        .collect::<Result<Vec<_>, DbErr>>()?;
    Ok(one_per_job(rows))
}

fn job_row(row: &QueryResult) -> Result<ExecJobRow, DbErr> {
    let job = MExecJob::from_query_result(row, "")?;
    Ok(ExecJobRow {
        status: job.status,
        job,
        entry_point: row.try_get("", "entry_point_name")?,
        last_phase: row.try_get("", "last_phase_name")?,
    })
}

fn one_per_job(rows: Vec<ExecJobRow>) -> Vec<ExecJobRow> {
    let mut by_entry_point: BTreeMap<EntryPointId, Vec<ExecJobRow>> = BTreeMap::new();
    for row in rows {
        by_entry_point
            .entry(row.job.entry_point)
            .or_default()
            .push(row);
    }
    let mut jobs: Vec<ExecJobRow> = by_entry_point
        .into_values()
        .filter_map(|mut nodes| {
            nodes.sort_by_key(|node| node.job.node_index);
            let statuses: Vec<ExecJobStatus> = nodes.iter().map(|node| node.job.status).collect();
            let mut master = nodes.into_iter().next()?;
            master.status = aggregate_status(&statuses);
            Some(master)
        })
        .collect();
    jobs.sort_by(|a, b| a.entry_point.cmp(&b.entry_point));
    jobs
}

pub async fn one<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
) -> Result<Option<ExecJobDetail>, DbErr> {
    let rows = nodes_of(db, job).await?;
    let Some(job) = rows.first().cloned() else {
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
    let node_ids: Vec<ExecJobId> = rows.iter().map(|row| row.id).collect();
    let all_attempts = EExecJobAttempt::find()
        .filter(CExecJobAttempt::ExecJob.is_in(node_ids))
        .order_by_asc(CExecJobAttempt::Number)
        .all(db)
        .await?;
    let nodes: Vec<ExecNodeDetail> = rows
        .into_iter()
        .map(|row| ExecNodeDetail {
            attempts: all_attempts
                .iter()
                .filter(|attempt| attempt.exec_job == row.id)
                .cloned()
                .collect(),
            job: row,
        })
        .collect();
    let statuses: Vec<ExecJobStatus> = nodes.iter().map(|node| node.job.status).collect();
    let attempts = nodes[0].attempts.clone();
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
    let tokens = super::tokens::of_job(db, job.id).await?;
    let started_evaluations = started_evaluations(db, job.id).await?;
    Ok(Some(ExecJobDetail {
        job,
        status: aggregate_status(&statuses),
        entry_point,
        task,
        project,
        attempts,
        phases,
        tokens,
        started_evaluations,
        nodes,
    }))
}

async fn started_evaluations<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
) -> Result<Vec<StartedEvaluation>, DbErr> {
    StartedEvaluation::find_by_statement(
        EVALUATIONS_STARTED_BY_EXEC_JOB.bind([Value::Uuid(Some(job.into_inner()))]),
    )
    .all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(entry_point: EntryPointId, name: &str, node: i16, status: ExecJobStatus) -> ExecJobRow {
        let job = MExecJob {
            id: ExecJobId::now_v7(),
            entry_point,
            node_index: node,
            status,
            ..Default::default()
        };
        ExecJobRow {
            status,
            job,
            entry_point: name.into(),
            last_phase: None,
        }
    }

    #[test]
    fn an_evaluation_lists_a_multi_node_job_once() {
        let (pair, single) = (EntryPointId::now_v7(), EntryPointId::now_v7());
        let rows = vec![
            row(single, "apps.x86_64-linux.a", 0, ExecJobStatus::Queued),
            row(pair, "apps.x86_64-linux.pair", 1, ExecJobStatus::Running),
            row(pair, "apps.x86_64-linux.pair", 0, ExecJobStatus::Completed),
        ];
        let master = rows[2].job.id;

        let jobs = one_per_job(rows);

        assert_eq!(
            jobs.iter()
                .map(|job| (job.entry_point.as_str(), job.status))
                .collect::<Vec<_>>(),
            vec![
                ("apps.x86_64-linux.a", ExecJobStatus::Queued),
                ("apps.x86_64-linux.pair", ExecJobStatus::Running),
            ]
        );
        assert_eq!(jobs[1].job.id, master, "the pair is listed under node 0");
    }
}
