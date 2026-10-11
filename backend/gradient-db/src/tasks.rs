/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, Select,
};

pub async fn by_name<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    name: &str,
) -> Result<Option<MTask>, DbErr> {
    ETask::find()
        .filter(CTask::Project.eq(project))
        .filter(CTask::Name.eq(name))
        .one(db)
        .await
}

pub fn of_project(project: ProjectId) -> Select<ETask> {
    ETask::find()
        .filter(CTask::Project.eq(project))
        .order_by_asc(CTask::Name)
        .order_by_asc(CTask::Id)
}

pub async fn insert<C: ConnectionTrait>(db: &C, task: MTask) -> Result<MTask, DbErr> {
    task.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(db: &C, task: ATask) -> Result<MTask, DbErr> {
    task.update(db).await
}

pub async fn delete<C: ConnectionTrait>(db: &C, task: MTask) -> Result<(), DbErr> {
    task.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn with_ids<C: ConnectionTrait>(db: &C, ids: &[TaskId]) -> Result<Vec<MTask>, DbErr> {
    crate::fetch_in_chunks(ids, |chunk| async move {
        ETask::find().filter(CTask::Id.is_in(chunk)).all(db).await
    })
    .await
}

pub async fn with_repository_containing<C: ConnectionTrait>(
    db: &C,
    fragment: &str,
) -> Result<Vec<MTask>, DbErr> {
    ETask::find()
        .filter(CTask::Repository.contains(fragment))
        .all(db)
        .await
}

pub async fn of_projects<C: ConnectionTrait>(
    db: &C,
    projects: Vec<ProjectId>,
) -> Result<Vec<MTask>, DbErr> {
    ETask::find()
        .filter(CTask::Project.is_in(projects))
        .all(db)
        .await
}
