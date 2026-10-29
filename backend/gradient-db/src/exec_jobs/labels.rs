/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::fetch_in_chunks;
use gradient_types::exec::ExecSpec;
use gradient_types::{
    CEntryPoint, CExecJob, CExecJobAttempt, CProject, CTask, DispatchedJobId, EEntryPoint,
    EExecJob, EExecJobAttempt, EProject, ETask, EntryPointId, ExecJobId, MExecJob, MExecJobAttempt,
    ProjectId, TaskId,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecJobLabel {
    pub id: ExecJobId,
    pub project: String,
    pub entry_point: String,
    pub task: String,
    pub node_index: u32,
    pub nodes: u32,
}

pub async fn labels<C: ConnectionTrait>(
    db: &C,
    jobs: &[ExecJobId],
) -> Result<Vec<ExecJobLabel>, DbErr> {
    let rows = fetch_in_chunks(jobs, |chunk| async move {
        EExecJob::find()
            .filter(CExecJob::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await?;
    let entry_points = entry_points_of(db, &rows).await?;
    let task_ids: Vec<TaskId> = distinct(entry_points.values().map(|(_, task)| *task));
    let tasks = task_names(db, &task_ids).await?;

    Ok(rows
        .into_iter()
        .filter_map(|job| {
            let (entry_point, task) = entry_points.get(&job.entry_point)?;
            let (project, task) = tasks.get(task).cloned().unwrap_or_default();
            Some(ExecJobLabel {
                id: job.id,
                project,
                entry_point: entry_point.clone(),
                task,
                node_index: u32::try_from(job.node_index).unwrap_or_default(),
                nodes: node_count(&job),
            })
        })
        .collect())
}

pub async fn attempt_of_assignment<C: ConnectionTrait>(
    db: &C,
    assignment: DispatchedJobId,
) -> Result<Option<MExecJobAttempt>, DbErr> {
    EExecJobAttempt::find()
        .filter(CExecJobAttempt::AssignmentId.eq(assignment))
        .one(db)
        .await
}

fn node_count(job: &MExecJob) -> u32 {
    serde_json::from_value::<ExecSpec>(job.spec.clone())
        .map(|spec| spec.nodes)
        .unwrap_or_default()
        .max(1)
}

fn distinct<T: Ord + Copy>(ids: impl Iterator<Item = T>) -> Vec<T> {
    ids.collect::<BTreeSet<_>>().into_iter().collect()
}

async fn entry_points_of<C: ConnectionTrait>(
    db: &C,
    rows: &[MExecJob],
) -> Result<HashMap<EntryPointId, (String, TaskId)>, DbErr> {
    let ids: Vec<EntryPointId> = distinct(rows.iter().map(|job| job.entry_point));
    let found = fetch_in_chunks(&ids, |chunk| async move {
        EEntryPoint::find()
            .select_only()
            .column(CEntryPoint::Eval)
            .column(CEntryPoint::Id)
            .column(CEntryPoint::Task)
            .filter(CEntryPoint::Id.is_in(chunk))
            .into_tuple::<(String, EntryPointId, TaskId)>()
            .all(db)
            .await
    })
    .await?;

    Ok(found
        .into_iter()
        .map(|(eval, id, task)| (id, (eval, task)))
        .collect())
}

async fn task_names<C: ConnectionTrait>(
    db: &C,
    ids: &[TaskId],
) -> Result<HashMap<TaskId, (String, String)>, DbErr> {
    let tasks = fetch_in_chunks(ids, |chunk| async move {
        ETask::find()
            .select_only()
            .column(CTask::Id)
            .column(CTask::Name)
            .column(CTask::Project)
            .filter(CTask::Id.is_in(chunk))
            .into_tuple::<(TaskId, String, ProjectId)>()
            .all(db)
            .await
    })
    .await?;
    let project_ids: Vec<ProjectId> = distinct(tasks.iter().map(|(_, _, project)| *project));
    let projects: HashMap<ProjectId, String> = fetch_in_chunks(&project_ids, |chunk| async move {
        EProject::find()
            .select_only()
            .column(CProject::Id)
            .column(CProject::Name)
            .filter(CProject::Id.is_in(chunk))
            .into_tuple::<(ProjectId, String)>()
            .all(db)
            .await
    })
    .await?
    .into_iter()
    .collect();

    Ok(tasks
        .into_iter()
        .map(|(id, task, project)| {
            let project = projects.get(&project).cloned().unwrap_or_default();
            (id, (project, task))
        })
        .collect())
}
