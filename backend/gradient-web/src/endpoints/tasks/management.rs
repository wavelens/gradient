/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::TaskResponse;
use super::auto_attach;
use super::refusal::TaskRefusal;
use crate::access::{Caller, ProjectAccess, has_permission, load_project};
use crate::api_spec::{ProjectPath, TaskPath};
use crate::audit::{Audit, changed_fields};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, paginate};
use crate::json_body::JsonBody;
use crate::names::{TaskDisplayName, TaskName};
use crate::permissions::Permission;
use crate::refusal::{Change, SharedRefusal};
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Readable, RequiresUnmanaged, TaskScope, can};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use gradient_db::lookup::{by_id, by_ids, get_any_project_by_name};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;

use gradient_core::ServerState;
use gradient_sources::check_task_updates;
use gradient_types::consts::*;
use gradient_types::input::vec_to_hex;
use gradient_types::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerType};
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeTaskRequest {
    pub name: TaskName,
    pub display_name: TaskDisplayName,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    #[serde(default)]
    pub concurrency: Option<ConcurrencyPolicy>,
    #[serde(default)]
    pub sign_cache: Option<bool>,
    #[serde(default)]
    pub wait_for_workers: Option<bool>,
    #[serde(default)]
    pub retry_failed_builds: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchTaskRequest {
    pub name: Option<TaskName>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub wildcard: Option<String>,
    pub keep_evaluations: Option<i32>,
    pub concurrency: Option<ConcurrencyPolicy>,
    pub sign_cache: Option<bool>,
    pub wait_for_workers: Option<bool>,
    pub retry_failed_builds: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TransferOwnershipRequest {
    pub project: String,
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/available",
    operation_id = "checkTaskNameAvailable",
    tag = "tasks",
    summary = "Check task name availability",
    description = "Returns `true` if no task with the given name exists in the project, `false` if it is already taken.",
    params(("project" = String, Path), ("name" = String, Query)),
    responses((status = OK, description = "Availability result", body = bool, content_type = "application/json"), WebError)
)]
pub async fn get_task_name_available(
    state: State<Arc<ServerState>>,
    Path(project): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_task_name(&name).is_err() {
        return Ok(ok_json(false));
    }
    let project = get_any_project_by_name(&state.db(), project)
        .await?
        .or_not_found(Resource::Project)?;
    let exists = gradient_db::tasks::by_name(&state.web_db, project.id, &name)
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

type PaginatedTaskResponse = Paginated<TaskResponse>;

#[utoipa::path(
    get,
    path = "/tasks/{project}",
    operation_id = "listTasks",
    tag = "tasks",
    summary = "List tasks",
    description = "Returns all tasks in the given project with full details.",
    params(ProjectPath, PaginationParams),
    responses((status = OK, description = "Task list", body = PaginatedTaskResponse), WebError)
)]
pub async fn get(
    ProjectScope {
        project,
        user: maybe_user,
        api_key,
        db,
        ..
    }: ProjectScope<Readable>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<TaskResponse>>>> {
    let api_key_ref = api_key.as_ref();

    let (can_edit, can_trigger) = match &maybe_user {
        Some(user) => (
            has_permission(
                &db.web_db,
                user.id,
                project.id,
                Permission::EditTask,
                api_key_ref,
            )
            .await?,
            has_permission(
                &db.web_db,
                user.id,
                project.id,
                Permission::TriggerEvaluation,
                api_key_ref,
            )
            .await?,
        ),
        None => (false, false),
    };

    let listing = paginate(
        gradient_db::tasks::of_project(project.id),
        &db.web_db,
        &params,
    )
    .await?;

    let eval_ids: Vec<EvaluationId> = listing
        .items
        .iter()
        .filter_map(|p| p.last_evaluation)
        .collect();
    let db = &db.web_db;
    let eval_status_map: HashMap<EvaluationId, gradient_entity::evaluation::EvaluationStatus> =
        by_ids::<EEvaluation, _>(db, &eval_ids)
            .await?
            .into_iter()
            .map(|(id, e)| (id, e.status))
            .collect();

    let listing = listing.map(|p| {
        let last_evaluation_status = p
            .last_evaluation
            .and_then(|id| eval_status_map.get(&id).cloned());
        TaskResponse::new(p, last_evaluation_status, can_edit, can_trigger)
    });

    Ok(ok_json(listing))
}

#[utoipa::path(
    put,
    path = "/tasks/{project}",
    operation_id = "createTask",
    tag = "tasks",
    summary = "Create task",
    description = "Creates a new task in the project. `wildcard` is a Nix attribute selector such as `packages.x86_64-linux` or `checks`.",
    params(("project" = String, Path)),
    request_body = MakeTaskRequest,
    responses((status = OK, description = "Task UUID", body = String, content_type = "application/json"), WebError)
)]
pub async fn put(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    JsonBody(body): JsonBody<MakeTaskRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    body.repository
        .parse::<RepositoryUrl>()
        .map_err(|e| TaskRefusal::InvalidRepositoryUrl(e.to_string()))?;

    let project = load_project(
        &state.0.db(),
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::CreateTask,
            reject_managed: true,
        },
    )
    .await?;

    let existing_task =
        gradient_db::tasks::by_name(&state.web_db, project.id, body.name.as_str()).await?;

    if existing_task.is_some() {
        return Err(WebError::already_exists(Resource::TaskName));
    }

    let wildcard = body
        .wildcard
        .trim()
        .parse::<Wildcard>()
        .map_err(|e| TaskRefusal::InvalidWildcard(e.to_string()))?
        .to_string();

    let task = MTask {
        id: TaskId::now_v7(),
        project: project.id,
        name: body.name.to_string(),
        active: true,
        display_name: body.display_name.trim().to_string(),
        description: body.description.trim().to_string(),
        repository: body.repository.clone(),
        wildcard,
        last_check_at: *NULL_TIME,
        created_by: user.id,
        created_at: gradient_types::now(),
        keep_evaluations: state.config.eval.default_keep_evaluations(),
        concurrency: body.concurrency.unwrap_or_default(),
        sign_cache: body.sign_cache.unwrap_or(true),
        wait_for_workers: body.wait_for_workers.unwrap_or(false),
        retry_failed_builds: body.retry_failed_builds.unwrap_or(true),
        ..Default::default()
    };

    let task = gradient_db::tasks::insert(&state.web_db, task)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TaskName))?;

    let now = gradient_types::now();
    let default_cfg = TriggerConfig::Polling {
        interval_secs: 300,
        branch: None,
    };
    let default_trigger = MTaskTrigger {
        id: TaskTriggerId::now_v7(),
        task: task.id,
        trigger_type: TriggerType::Polling,
        config: default_cfg.to_db_json(),
        active: true,
        created_at: now,
        updated_at: now,
        ..Default::default()
    };
    gradient_db::triggers::insert(&state.web_db, default_trigger).await?;

    let integrations = gradient_db::integrations::of_project(&state.web_db, project.id)
        .await
        .unwrap_or_default();
    if let Err(e) = auto_attach::apply(&state.web_db, &task, &integrations).await {
        tracing::warn!(
            "auto-attaching integrations to task {} failed: {e}",
            task.id
        );
    }

    audit
        .write(
            Action::TaskCreate,
            EventOwner::task(project.id, task.id),
            serde_json::json!({ "task_id": task.id.to_string(), "name": task.name, }),
        )
        .await;

    let res = BaseResponse {
        error: false,
        message: task.id.to_string(),
    };

    Ok(Json(res))
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}",
    operation_id = "getTask",
    tag = "tasks",
    summary = "Get task",
    params(TaskPath),
    responses((status = OK, description = "Task details", body = TaskResponse), WebError)
)]
pub async fn get_task(
    TaskScope {
        project,
        task,
        user: maybe_user,
        api_key,
        db,
        ..
    }: TaskScope<Readable>,
) -> WebResult<Json<BaseResponse<TaskResponse>>> {
    let api_key_ref = api_key.as_ref();

    let (can_edit, can_trigger) = match &maybe_user {
        Some(user) => (
            has_permission(
                &db.web_db,
                user.id,
                project.id,
                Permission::EditTask,
                api_key_ref,
            )
            .await?,
            has_permission(
                &db.web_db,
                user.id,
                project.id,
                Permission::TriggerEvaluation,
                api_key_ref,
            )
            .await?,
        ),
        None => (false, false),
    };

    let last_evaluation_status = if let Some(eval_id) = task.last_evaluation {
        by_id::<EEvaluation, _>(&db.web_db, eval_id)
            .await?
            .map(|e| e.status)
    } else {
        None
    };

    Ok(ok_json(TaskResponse::new(
        task,
        last_evaluation_status,
        can_edit,
        can_trigger,
    )))
}

#[utoipa::path(
    patch,
    path = "/tasks/{project}/{task}",
    operation_id = "patchTask",
    tag = "tasks",
    summary = "Update task",
    description = "Updates one or more task fields.",
    params(TaskPath),
    request_body = PatchTaskRequest,
    responses((status = OK, description = "Updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_task(
    TaskScope { project, task, .. }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<PatchTaskRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let owner = EventOwner::task(project.id, task.id);
    let fields = changed_fields([
        ("name", body.name.is_some()),
        ("display_name", body.display_name.is_some()),
        ("description", body.description.is_some()),
        ("repository", body.repository.is_some()),
        ("wildcard", body.wildcard.is_some()),
        ("keep_evaluations", body.keep_evaluations.is_some()),
        ("concurrency", body.concurrency.is_some()),
        ("sign_cache", body.sign_cache.is_some()),
        ("wait_for_workers", body.wait_for_workers.is_some()),
        ("retry_failed_builds", body.retry_failed_builds.is_some()),
    ]);
    let mut atask: ATask = task.into();
    let mut patcher = TaskPatcher::new(&state, &mut atask);

    if let Some(name) = body.name {
        patcher.apply_name(&project, name).await?;
    }
    if let Some(display_name) = body.display_name {
        patcher.apply_display_name(display_name)?;
    }
    if let Some(description) = body.description {
        patcher.atask.description = Set(description.trim().to_string());
    }
    if let Some(repository) = body.repository {
        patcher.apply_repository(repository)?;
    }
    if let Some(wildcard) = body.wildcard {
        patcher.apply_wildcard(wildcard)?;
    }
    if let Some(keep) = body.keep_evaluations {
        patcher.apply_keep_evaluations(keep)?;
    }
    if let Some(concurrency) = body.concurrency {
        patcher.apply_concurrency(concurrency)?;
    }
    if let Some(sign_cache) = body.sign_cache {
        patcher.apply_sign_cache(sign_cache);
    }
    if let Some(wait_for_workers) = body.wait_for_workers {
        patcher.atask.wait_for_workers = Set(wait_for_workers);
    }
    if let Some(retry_failed_builds) = body.retry_failed_builds {
        patcher.atask.retry_failed_builds = Set(retry_failed_builds);
    }

    atask.force_evaluation = Set(true);
    gradient_db::tasks::update(&state.web_db, atask)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TaskName))?;

    audit.write(Action::TaskUpdate, owner, fields).await;

    Ok(ok_json("Task updated".to_string()))
}

struct TaskPatcher<'a> {
    state: &'a State<Arc<ServerState>>,
    atask: &'a mut ATask,
}

impl<'a> TaskPatcher<'a> {
    fn new(state: &'a State<Arc<ServerState>>, atask: &'a mut ATask) -> Self {
        Self { state, atask }
    }

    async fn apply_name(&mut self, project: &MProject, name: TaskName) -> WebResult<()> {
        let existing =
            gradient_db::tasks::by_name(&self.state.web_db, project.id, name.as_str()).await?;
        if existing.is_some() {
            return Err(WebError::already_exists(Resource::TaskName));
        }
        self.atask.name = Set(name.into_inner());
        Ok(())
    }

    fn apply_display_name(&mut self, display_name: String) -> WebResult<()> {
        let display_name = TaskDisplayName::try_from(display_name.trim().to_string())?;
        self.atask.display_name = Set(display_name.into_inner());
        Ok(())
    }

    fn apply_repository(&mut self, repository: String) -> WebResult<()> {
        repository
            .parse::<RepositoryUrl>()
            .map_err(|e| TaskRefusal::InvalidRepositoryUrl(e.to_string()))?;
        self.atask.repository = Set(repository);
        Ok(())
    }

    fn apply_wildcard(&mut self, wildcard: String) -> WebResult<()> {
        let wildcard = wildcard
            .trim()
            .parse::<Wildcard>()
            .map_err(|e| TaskRefusal::InvalidWildcard(e.to_string()))?
            .to_string();
        self.atask.wildcard = Set(wildcard);
        Ok(())
    }

    fn apply_keep_evaluations(&mut self, keep: i32) -> WebResult<()> {
        if keep < 1 {
            return Err(TaskRefusal::KeepEvaluationsBelowMinimum.into());
        }
        if let Some(global_max) = self.state.config.eval.keep_evaluations_max()
            && keep > global_max
        {
            return Err(TaskRefusal::KeepEvaluationsAboveMaximum {
                maximum: global_max,
            }
            .into());
        }
        self.atask.keep_evaluations = Set(keep);
        Ok(())
    }

    fn apply_concurrency(&mut self, concurrency: ConcurrencyPolicy) -> WebResult<()> {
        self.atask.concurrency = Set(concurrency);
        Ok(())
    }

    fn apply_sign_cache(&mut self, sign_cache: bool) {
        self.atask.sign_cache = Set(sign_cache);
    }
}

#[utoipa::path(
    delete,
    path = "/tasks/{project}/{task}",
    operation_id = "deleteTask",
    tag = "tasks",
    summary = "Delete task",
    params(TaskPath),
    responses((status = OK, description = "Deleted", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_task(
    TaskScope {
        project: project_row,
        task,
        db,
        ..
    }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    let task_id = task.id;
    let task_name = task.name.clone();
    gradient_db::tasks::delete(&db.web_db, task).await?;

    audit.write(Action::TaskDelete, EventOwner::task(project_row.id, task_id), serde_json::json!({ "project_id": project_row.id.to_string(), "task_id": task_id.to_string(), "task_name": task_name, }))
    .await;

    let res = BaseResponse {
        error: false,
        message: "Task deleted".to_string(),
    };

    Ok(Json(res))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/active",
    operation_id = "enableTask",
    tag = "tasks",
    summary = "Enable task",
    description = "Sets the task's active flag to true so it will be evaluated on new commits. Requires Admin or Write role; rejects state-managed tasks.",
    params(TaskPath),
    responses((status = OK, description = "Enabled", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_task_active(
    TaskScope { task, db, .. }: TaskScope<RequiresUnmanaged<can::EditTask>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let mut atask: ATask = task.into();
    atask.active = Set(true);
    gradient_db::tasks::update(&db.web_db, atask).await?;

    let res = BaseResponse {
        error: false,
        message: "Task enabled".to_string(),
    };

    Ok(Json(res))
}

#[utoipa::path(
    delete,
    path = "/tasks/{project}/{task}/active",
    operation_id = "disableTask",
    tag = "tasks",
    summary = "Disable task",
    description = "Sets the task's active flag to false, pausing automatic evaluations. Requires Admin or Write role; rejects state-managed tasks.",
    params(TaskPath),
    responses((status = OK, description = "Disabled", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_task_active(
    TaskScope { task, db, .. }: TaskScope<RequiresUnmanaged<can::EditTask>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let mut atask: ATask = task.into();
    atask.active = Set(false);
    gradient_db::tasks::update(&db.web_db, atask).await?;

    let res = BaseResponse {
        error: false,
        message: "Task disabled".to_string(),
    };

    Ok(Json(res))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/check-repository",
    operation_id = "checkTaskRepository",
    tag = "tasks",
    summary = "Check repository accessibility",
    description = "Tests whether Gradient can clone the task's repository using the project's SSH key. Requires Admin or Write role; rejects state-managed tasks.\nReturns `400` with `code: \"repository_unreachable\"` and the underlying git error when the repository cannot be fetched.",
    params(TaskPath),
    responses((status = OK, description = "Repository is accessible", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_task_check_repository(
    TaskScope { task, db, .. }: TaskScope<RequiresUnmanaged<can::EditTask>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_has_updates, remote_hash) = check_task_updates(&db, &task, None).await.map_err(|e| {
        TaskRefusal::RepositoryCheckFailed {
            reason: e.to_string(),
        }
    })?;

    let res = BaseResponse {
        error: false,
        message: vec_to_hex(&remote_hash),
    };

    Ok(Json(res))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/transfer",
    operation_id = "transferTask",
    tag = "tasks",
    summary = "Move task to another project",
    description = "Moves the task into the project named in the body. Allowed for the task owner or a member with the edit-task permission, who also needs the create-task permission in the target project. Rejects state-managed tasks and target projects.",
    params(TaskPath),
    request_body = TransferOwnershipRequest,
    responses((status = OK, description = "Task moved", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_task_transfer(
    TaskScope {
        project,
        task,
        user,
        api_key,
        db,
        ..
    }: TaskScope<Member>,
    Json(body): Json<TransferOwnershipRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key_ref = api_key.as_ref();

    let is_admin = has_permission(
        &db.web_db,
        user.id,
        project.id,
        Permission::EditTask,
        api_key_ref,
    )
    .await?;
    let is_owner = task.created_by == user.id;
    if !is_admin && !is_owner {
        return Err(TaskRefusal::TransferNotPermitted.into());
    }

    if task.managed {
        return Err(SharedRefusal::StateManaged(Change::TransferOwnership, Resource::Task).into());
    }

    let new_project = load_project(
        &db,
        Caller::User(&user),
        api_key_ref,
        body.project.clone(),
        ProjectAccess::Require {
            permission: Permission::CreateTask,
            reject_managed: true,
        },
    )
    .await?;

    if new_project.id == project.id {
        return Err(TaskRefusal::AlreadyInProject.into());
    }

    gradient_db::tasks::move_to_project(&db.web_db, task, new_project.id)
        .await?
        .or_not_found(Resource::Task)?;

    let res = BaseResponse {
        error: false,
        message: "Ownership transferred".to_string(),
    };

    Ok(Json(res))
}
