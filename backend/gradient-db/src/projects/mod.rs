/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod caches;
pub mod derivations;
pub mod workers;

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::Query;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Select, Value,
};
use std::collections::HashMap;

pub async fn by_name<C: ConnectionTrait>(db: &C, name: &str) -> Result<Option<MProject>, DbErr> {
    EProject::find()
        .filter(CProject::Name.eq(name))
        .one(db)
        .await
}

pub fn reachable_by_user(user: UserId) -> Select<EProject> {
    let reachable = Query::select()
        .column(CProjectAccess::Project)
        .from(gradient_entity::project_access::Entity)
        .and_where(CProjectAccess::User.eq(user))
        .to_owned();
    EProject::find()
        .filter(CProject::Id.in_subquery(reachable))
        .order_by_asc(CProject::Name)
        .order_by_asc(CProject::Id)
}

pub fn all_public() -> Select<EProject> {
    EProject::find()
        .filter(CProject::Public.eq(true))
        .order_by_asc(CProject::Name)
        .order_by_asc(CProject::Id)
}

pub async fn running_evaluations_per_project<C: ConnectionTrait>(
    db: &C,
    project_ids: &[ProjectId],
) -> Result<HashMap<ProjectId, i64>, DbErr> {
    if project_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let tasks = ETask::find()
        .filter(CTask::Project.is_in(project_ids.to_vec()))
        .all(db)
        .await?;

    let task_ids: Vec<TaskId> = tasks.iter().map(|p| p.id).collect();
    let task_to_project: HashMap<TaskId, ProjectId> =
        tasks.into_iter().map(|p| (p.id, p.project)).collect();

    let mut running_per_project: HashMap<ProjectId, i64> = HashMap::new();
    if !task_ids.is_empty() {
        let running = EEvaluation::find()
            .filter(CEvaluation::Task.is_in(task_ids))
            .filter(CEvaluation::Status.is_in(EvaluationStatus::ACTIVE))
            .all(db)
            .await?;
        for eval in running {
            if let Some(task_id) = eval.task
                && let Some(&project_id) = task_to_project.get(&task_id)
            {
                *running_per_project.entry(project_id).or_insert(0) += 1;
            }
        }
    }

    Ok(running_per_project)
}

pub async fn set_public<C: ConnectionTrait>(
    db: &C,
    project: MProject,
    public: bool,
) -> Result<MProject, DbErr> {
    let mut active: AProject = project.into();
    active.public = Set(public);
    active.update(db).await
}

pub async fn replace_ssh_key<C: ConnectionTrait>(
    db: &C,
    project: MProject,
    private_key: String,
    public_key: String,
) -> Result<MProject, DbErr> {
    let mut active: AProject = project.into();
    active.private_key = Set(private_key);
    active.public_key = Set(public_key);
    active.update(db).await
}

pub async fn access_of_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<MProjectAccess>, DbErr> {
    EProjectAccess::find()
        .filter(CProjectAccess::User.eq(user))
        .all(db)
        .await
}

pub async fn with_ids<C: ConnectionTrait>(
    db: &C,
    ids: &[ProjectId],
) -> Result<Vec<MProject>, DbErr> {
    crate::fetch_in_chunks(ids, |chunk| async move {
        EProject::find()
            .filter(CProject::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await
}

pub async fn access_of_user_in<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    projects: Vec<ProjectId>,
) -> Result<Option<MProjectAccess>, DbErr> {
    EProjectAccess::find()
        .filter(CProjectAccess::User.eq(user))
        .filter(CProjectAccess::Project.is_in(projects))
        .one(db)
        .await
}

pub async fn members_with_role<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    role: RoleId,
) -> Result<Vec<MProjectUser>, DbErr> {
    EProjectUser::find()
        .filter(CProjectUser::Project.eq(project))
        .filter(CProjectUser::Role.eq(role))
        .all(db)
        .await
}

crate::sql! {
    PUBLIC_PROJECTS = "SELECT id FROM project WHERE public = true",
        params = [];

    PROJECTS_FOR_USER = "SELECT DISTINCT project AS id FROM project_access WHERE \"user\" = $1",
        params = [UserId];
}

pub async fn public_ids<C: ConnectionTrait>(db: &C) -> Result<Vec<ProjectId>, DbErr> {
    db.query_all_raw(PUBLIC_PROJECTS.stmt())
        .await?
        .into_iter()
        .map(|row| row.try_get::<uuid::Uuid>("", "id").map(ProjectId::new))
        .collect()
}

pub async fn ids_reachable_by<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<ProjectId>, DbErr> {
    db.query_all_raw(PROJECTS_FOR_USER.bind([Value::from(uuid::Uuid::from(user))]))
        .await?
        .into_iter()
        .map(|row| row.try_get::<uuid::Uuid>("", "id").map(ProjectId::new))
        .collect()
}

pub async fn names<C: ConnectionTrait>(
    db: &C,
    projects: &[uuid::Uuid],
) -> Result<Vec<(uuid::Uuid, String, String)>, DbErr> {
    crate::fetch_in_chunks(projects, |chunk| async move {
        EProject::find()
            .select_only()
            .column(CProject::Id)
            .column(CProject::Name)
            .column(CProject::DisplayName)
            .filter(CProject::Id.is_in(chunk))
            .into_tuple()
            .all(db)
            .await
    })
    .await
}
