/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::TaskResponse;
use super::integration_by_repository_host;
use super::refusal::TaskRefusal;
use super::triggers::{
    build_task_trigger_row_from_config_and_flags,
    validate_trigger_config_and_return_config_to_store,
};
use crate::access::{Caller, ProjectAccess, caller_project_mask, has_permission, load_project};
use crate::api_spec::{ProjectPath, TaskPath};
use crate::audit::{Audit, changed_fields};
use crate::authorization::MaybeApiKey;
use crate::endpoints::evals::task_run_history::get_last_n_evaluation_runs_by_latest_evaluation_id;
use crate::error::{WebError, WebResult};
use crate::helpers::{CreatedEntityResponse, OptionExt, ok_json, paginate};
use crate::json_body::JsonBody;
use crate::names::{TaskDisplayName, TaskName};
use crate::permissions::Permission;
use crate::refusal::{Change, SharedRefusal};
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Readable, Requires, RequiresUnmanaged, TaskScope, can};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use gradient_db::WebDb;
use gradient_types::actions::ActionConfig;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;

use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_sources::resolve_remote_ref;
use gradient_types::consts::*;
use gradient_types::input::vec_to_hex;
use gradient_types::triggers::{ConcurrencyPolicy, TriggerConfig};
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

const TASK_RECENT_EVALUATION_RUN_COUNT: u64 = 5;

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
    pub wait_for_workers: Option<bool>,
    #[serde(default)]
    pub retry_failed_builds: Option<bool>,
    #[serde(default)]
    pub multi_node_evaluation: Option<bool>,
    #[serde(default)]
    pub exec_enabled: Option<bool>,
    #[serde(default)]
    pub triggers: Option<Vec<TriggerConfig>>,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct CheckRepositoryRequest {
    pub repository: Option<String>,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct RepositoryAccessCheckRequest {
    pub repository: String,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct DuplicateTaskRequest {
    pub name: TaskName,
    pub display_name: TaskDisplayName,
    pub branch: String,
}

async fn fetch_remote_repository_head_commit_with_project_ssh_key(
    db: &DbContext,
    project: &MProject,
    repository: &str,
) -> Result<Vec<u8>, String> {
    resolve_remote_ref(db, project, repository, None)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, %repository, "Repository check failed");
            e.short_reason_for_user()
        })
}

fn validated_repository(repository: String) -> WebResult<String> {
    let repository = repository.trim();
    repository
        .parse::<RepositoryUrl>()
        .map_err(|e| TaskRefusal::InvalidRepositoryUrl(e.to_string()))?;
    Ok(repository.to_owned())
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
    pub wait_for_workers: Option<bool>,
    pub retry_failed_builds: Option<bool>,
    pub multi_node_evaluation: Option<bool>,
    pub exec_enabled: Option<bool>,
    pub only_evaluate: Option<bool>,
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
    ProjectScope { project, db, .. }: ProjectScope<Member>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_task_name(&name).is_err() {
        return Ok(ok_json(false));
    }
    let exists = gradient_db::tasks::by_name(&db.web_db, project.id, &name)
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
    let permissions = caller_project_mask(
        &db.web_db,
        Caller::from(&maybe_user),
        project.id,
        api_key.as_ref(),
    )
    .await?;

    let listing = paginate(
        gradient_db::tasks::of_project(project.id),
        &db.web_db,
        &params,
    )
    .await?;

    let db = &db.web_db;
    let task_ids: Vec<TaskId> = listing.items.iter().map(|p| p.id).collect();
    let latest = gradient_db::evaluations::latest_evaluation_status_of_tasks(db, &task_ids).await?;
    let latest_ids: Vec<EvaluationId> = latest.values().map(|e| e.id).collect();
    let mut recent_evaluations = get_last_n_evaluation_runs_by_latest_evaluation_id(
        db,
        &latest_ids,
        TASK_RECENT_EVALUATION_RUN_COUNT,
    )
    .await?;
    let triggers = gradient_db::triggers::of_tasks(db, &task_ids).await?;

    let listing = listing.map(|p| {
        let latest_evaluation = latest.get(&p.id).copied();
        let recent_evaluations = latest_evaluation
            .and_then(|e| recent_evaluations.remove(&e.id))
            .unwrap_or_default();
        TaskResponse::new(
            p,
            latest_evaluation,
            recent_evaluations,
            &triggers,
            permissions,
        )
    });

    Ok(ok_json(listing))
}

#[utoipa::path(
    put,
    path = "/tasks/{project}",
    operation_id = "createTask",
    tag = "tasks",
    summary = "Create task",
    description = "Create a new task in a project. `wildcard` is a selector for an attribute in Nix, such as `packages.x86_64-linux` or `checks`. Entries in `triggers` list the triggers for the new task. The first entry is the default trigger. New tasks with no `triggers` get one polling trigger for the default branch of the repository. Use an empty list to create a task with no triggers. Project integrations add no triggers and no actions.",
    params(("project" = String, Path)),
    request_body = MakeTaskRequest,
    responses((status = OK, description = "Created a task.", body = CreatedEntityResponse), WebError)
)]
pub async fn put(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    JsonBody(body): JsonBody<MakeTaskRequest>,
) -> WebResult<Json<BaseResponse<CreatedEntityResponse>>> {
    let repository = validated_repository(body.repository.clone())?;

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

    let trigger_configs = match body.triggers {
        None => vec![TriggerConfig::Polling {
            interval_secs: 300,
            branch: None,
        }],
        Some(configs) => {
            let mut validated = Vec::with_capacity(configs.len());
            for config in configs {
                validated.push(
                    validate_trigger_config_and_return_config_to_store(
                        &state.web_db,
                        project.id,
                        config,
                    )
                    .await?,
                );
            }
            validated
        }
    };

    let now = gradient_types::now();
    let task = MTask {
        id: TaskId::now_v7(),
        project: project.id,
        name: body.name.to_string(),
        active: true,
        display_name: body.display_name.trim().to_string(),
        description: body.description.trim().to_string(),
        repository,
        wildcard,
        last_check_at: *NULL_TIME,
        created_by: user.id,
        created_at: now,
        keep_evaluations: state.config.eval.default_keep_evaluations(),
        concurrency: body.concurrency.unwrap_or_default(),
        wait_for_workers: body.wait_for_workers.unwrap_or(false),
        retry_failed_builds: body.retry_failed_builds.unwrap_or(true),
        multi_node_evaluation: body.multi_node_evaluation.unwrap_or(false),
        exec_enabled: body.exec_enabled.unwrap_or(false),
        ..Default::default()
    };

    let txn = state.web_db.inner().begin().await?;
    let task = gradient_db::tasks::insert(&txn, task)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TaskName))?;
    for (position, config) in trigger_configs.iter().enumerate() {
        let row =
            build_task_trigger_row_from_config_and_flags(task.id, config, true, position == 0, now);
        gradient_db::triggers::insert(&txn, row).await?;
    }
    txn.commit().await?;

    let github_app =
        integration_by_repository_host::configured_github_app_if_caller_can_manage_integrations(
            &state.web_db,
            state.config.github_app.as_ref(),
            user.id,
            project.id,
            api_key.as_ref(),
        )
        .await?;
    integration_by_repository_host::link_project_to_github_app_installation_by_repository_host(
        &state.web_db,
        &state.http,
        github_app,
        &task,
    )
    .await;

    audit
        .write(
            Action::TaskCreate,
            EventOwner::task(project.id, task.id),
            serde_json::json!({ "task_id": task.id.to_string(), "name": task.name, }),
        )
        .await;

    Ok(ok_json(CreatedEntityResponse::new(
        task.id,
        task.name.clone(),
        frontend_page_urls::task(&state.config.server.frontend_url, &project.name, &task.name),
    )))
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
    let permissions = caller_project_mask(
        &db.web_db,
        Caller::from(&maybe_user),
        project.id,
        api_key.as_ref(),
    )
    .await?;

    let latest_evaluation =
        gradient_db::evaluations::latest_evaluation_status_of_tasks(&db.web_db, &[task.id])
            .await?
            .remove(&task.id);
    let recent_evaluations = match latest_evaluation {
        Some(latest) => get_last_n_evaluation_runs_by_latest_evaluation_id(
            &db.web_db,
            &[latest.id],
            TASK_RECENT_EVALUATION_RUN_COUNT,
        )
        .await?
        .remove(&latest.id)
        .unwrap_or_default(),
        None => Vec::new(),
    };
    let triggers = gradient_db::triggers::of_task(&db.web_db, task.id).await?;

    Ok(ok_json(TaskResponse::new(
        task,
        latest_evaluation,
        recent_evaluations,
        &triggers,
        permissions,
    )))
}

#[utoipa::path(
    patch,
    path = "/tasks/{project}/{task}",
    operation_id = "patchTask",
    tag = "tasks",
    summary = "Update task",
    description = "Update one or more fields of a task. Changes to `repository`, `wildcard`, `only_evaluate`, `exec_enabled` or `multi_node_evaluation` trigger a re-evaluation of the latest commit. Other fields take effect at the next evaluation.",
    params(TaskPath),
    request_body = PatchTaskRequest,
    responses((status = OK, description = "Updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_task(
    TaskScope {
        project,
        task,
        user,
        api_key,
        db,
        ..
    }: TaskScope<RequiresUnmanaged<can::EditTask>>,
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
        ("wait_for_workers", body.wait_for_workers.is_some()),
        ("retry_failed_builds", body.retry_failed_builds.is_some()),
        (
            "multi_node_evaluation",
            body.multi_node_evaluation.is_some(),
        ),
        ("exec_enabled", body.exec_enabled.is_some()),
        ("only_evaluate", body.only_evaluate.is_some()),
    ]);
    let repository_changed = body
        .repository
        .as_deref()
        .is_some_and(|url| url.trim() != task.repository);
    let evaluation_inputs_changed = repository_changed
        || body.wildcard.is_some()
        || body.only_evaluate.is_some()
        || body.exec_enabled.is_some()
        || body.multi_node_evaluation.is_some();
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
    if let Some(wait_for_workers) = body.wait_for_workers {
        patcher.atask.wait_for_workers = Set(wait_for_workers);
    }
    if let Some(retry_failed_builds) = body.retry_failed_builds {
        patcher.atask.retry_failed_builds = Set(retry_failed_builds);
    }
    if let Some(multi_node_evaluation) = body.multi_node_evaluation {
        patcher.atask.multi_node_evaluation = Set(multi_node_evaluation);
    }
    if let Some(exec_enabled) = body.exec_enabled {
        patcher.atask.exec_enabled = Set(exec_enabled);
    }
    if let Some(only_evaluate) = body.only_evaluate {
        patcher.atask.only_evaluate = Set(only_evaluate);
    }

    if evaluation_inputs_changed {
        atask.force_evaluation = Set(true);
    }
    if repository_changed {
        atask.last_fetch_at = Set(None);
        atask.last_fetch_ok_at = Set(None);
        atask.last_fetch_error = Set(None);
    }
    let updated = gradient_db::tasks::update(&state.web_db, atask)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TaskName))?;

    if repository_changed {
        let github_app = integration_by_repository_host::configured_github_app_if_caller_can_manage_integrations(
            &db.web_db,
            state.config.github_app.as_ref(),
            user.id,
            project.id,
            api_key.as_ref(),
        )
        .await?;
        integration_by_repository_host::link_project_to_github_app_installation_by_repository_host(
            &state.web_db,
            &state.http,
            github_app,
            &updated,
        )
        .await;
    }

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
        if existing.is_some_and(|existing| &existing.id != self.atask.id.as_ref()) {
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
        self.atask.repository = Set(validated_repository(repository)?);
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
    scope: TaskScope<RequiresUnmanaged<can::EditTask>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    set_task_active_flag_and_write_audit_log(scope, &audit, true).await
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
    scope: TaskScope<RequiresUnmanaged<can::EditTask>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    set_task_active_flag_and_write_audit_log(scope, &audit, false).await
}

async fn set_task_active_flag_and_write_audit_log(
    TaskScope { task, db, .. }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    audit: &Audit,
    active: bool,
) -> WebResult<Json<BaseResponse<String>>> {
    let owner = EventOwner::task(task.project, task.id);
    let mut atask: ATask = task.into();
    atask.active = Set(active);
    gradient_db::tasks::update(&db.web_db, atask).await?;
    audit
        .write(
            Action::TaskActiveFlagChanged,
            owner,
            serde_json::json!({ "active": active }),
        )
        .await;

    let message = if active {
        "Task enabled"
    } else {
        "Task disabled"
    };
    Ok(ok_json(message.to_string()))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/check-repository",
    operation_id = "checkTaskRepository",
    tag = "tasks",
    summary = "Check repository accessibility",
    description = "Tests whether Gradient can clone the task's repository using the project's SSH key and returns the remote head commit. A `repository` in the body is tested instead of the saved URL, so a new URL can be checked before saving it. A check of the saved URL updates the `repository_fetch` of the task. Requires Admin or Write role; rejects state-managed tasks.\nThe endpoint can return ``400`` with ``code: \"repository_unreachable\"`` and a short reason. Repository fetch failures can have one of three causes - no repository at the URL, denied access, or an unreachable server.",
    params(TaskPath),
    request_body = Option<CheckRepositoryRequest>,
    responses((status = OK, description = "Repository is accessible", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_task_check_repository(
    TaskScope {
        project, task, db, ..
    }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    body: Option<Json<Option<CheckRepositoryRequest>>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let requested = body.and_then(|Json(b)| b?.repository);
    let checks_saved_url = requested
        .as_deref()
        .is_none_or(|url| url.trim() == task.repository);
    let repository = validated_repository(requested.unwrap_or_else(|| task.repository.clone()))?;
    let result =
        fetch_remote_repository_head_commit_with_project_ssh_key(&db, &project, &repository).await;
    if checks_saved_url {
        gradient_db::tasks::record_fetch(
            &db.web_db,
            task.id,
            gradient_types::now(),
            result.as_ref().err().map(String::as_str),
        )
        .await?;
    }
    let head = result.map_err(|reason| TaskRefusal::RepositoryCheckFailed { reason })?;

    let res = BaseResponse {
        error: false,
        message: vec_to_hex(&head),
    };

    Ok(Json(res))
}

#[utoipa::path(
    post,
    path = "/projects/{project}/repository-check",
    operation_id = "checkProjectRepository",
    tag = "tasks",
    summary = "Check repository access before creating a task",
    description = "Test whether Gradient can clone `repository` with the SSH key of the project. Return the head commit of the remote. Use this check before task creation, for example in the task wizard. Callers need create-task permission. Reject state-managed projects. Return `400` with `code: \"repository_unreachable\"` and a short reason after a failed check.",
    params(ProjectPath),
    request_body = RepositoryAccessCheckRequest,
    responses((status = OK, description = "Repository is accessible", body = String, content_type = "application/json"), WebError)
)]
pub async fn check_project_repository_can_be_cloned(
    ProjectScope { project, db, .. }: ProjectScope<RequiresUnmanaged<can::CreateTask>>,
    JsonBody(body): JsonBody<RepositoryAccessCheckRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let repository = validated_repository(body.repository.trim().to_owned())?;
    let head = fetch_remote_repository_head_commit_with_project_ssh_key(&db, &project, &repository)
        .await
        .map_err(|reason| TaskRefusal::RepositoryCheckFailed { reason })?;
    Ok(ok_json(vec_to_hex(&head)))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/duplicate",
    operation_id = "duplicateTask",
    tag = "tasks",
    summary = "Duplicate task",
    description = "Create a copy of a task with a new `name` and `display_name`. Task settings, exec settings, triggers, and actions carry over to the copy. All triggers in the copy run with `branch`. Requests need create-task permission in the project. Requests for state-managed projects fail. Return the UUID, the name, and the URL of the web page for the new task.",
    params(TaskPath),
    request_body = DuplicateTaskRequest,
    responses((status = OK, description = "Created a copy of the task.", body = CreatedEntityResponse), WebError)
)]
pub async fn duplicate_task(
    state: State<Arc<ServerState>>,
    TaskScope {
        project,
        task,
        user,
        db,
        ..
    }: TaskScope<Requires<can::CreateTask>>,
    audit: Audit,
    JsonBody(body): JsonBody<DuplicateTaskRequest>,
) -> WebResult<Json<BaseResponse<CreatedEntityResponse>>> {
    if project.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Project).into());
    }
    let branch = body.branch.trim();
    if branch.is_empty() {
        return Err(SharedRefusal::EmptyField("branch").into());
    }
    if gradient_db::tasks::by_name(&db.web_db, project.id, body.name.as_str())
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::TaskName));
    }

    let now = gradient_types::now();
    let source_id = task.id;
    let copy = MTask {
        id: TaskId::now_v7(),
        name: body.name.into_inner(),
        display_name: body.display_name.trim().to_string(),
        last_evaluation: None,
        last_check_at: *NULL_TIME,
        force_evaluation: false,
        created_by: user.id,
        created_at: now,
        managed: false,
        last_fetch_at: None,
        last_fetch_ok_at: None,
        last_fetch_error: None,
        ..task
    };
    let triggers = gradient_db::triggers::of_task(&db.web_db, source_id).await?;
    let actions = gradient_db::actions::of_task(&db.web_db, source_id).await?;

    let txn = db.web_db.begin().await?;
    let copy = gradient_db::tasks::insert(&txn, copy)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TaskName))?;
    for trigger in triggers {
        let config = match TriggerConfig::parse_row(trigger.trigger_type, &trigger.config) {
            Ok(config) => config.with_branch(branch.to_owned()).to_db_json(),
            Err(_) => trigger.config.clone(),
        };
        let row = MTaskTrigger {
            id: TaskTriggerId::now_v7(),
            task: copy.id,
            config,
            last_fired_at: None,
            created_at: now,
            updated_at: now,
            ..trigger
        };
        gradient_db::triggers::insert(&txn, row).await?;
    }
    for action in actions {
        let row = MTaskAction {
            id: TaskActionId::now_v7(),
            task: copy.id,
            last_fired_at: None,
            created_by: user.id,
            created_at: now,
            updated_at: now,
            ..action
        };
        gradient_db::actions::insert(&txn, row).await?;
    }
    txn.commit().await?;

    audit
        .write(
            Action::TaskCreate,
            EventOwner::task(project.id, copy.id),
            serde_json::json!({ "task_id": copy.id.to_string(), "name": copy.name, "duplicate_of": source_id.to_string() }),
        )
        .await;

    Ok(ok_json(CreatedEntityResponse::new(
        copy.id,
        copy.name.clone(),
        frontend_page_urls::task(&state.config.server.frontend_url, &project.name, &copy.name),
    )))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/transfer",
    operation_id = "transferTask",
    tag = "tasks",
    summary = "Move task to another project",
    description = "Moves the task into the project named in the body. Allowed for the task owner or a member with the edit-task permission, who also needs the create-task permission in the target project. Rejects state-managed tasks and target projects. Reject tasks that use an integration with a Git host in a trigger or action. Return `409` for a name already taken in the target project.",
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
    audit: Audit,
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
    if task_references_git_host_integration(&db.web_db, task.id).await? {
        return Err(TaskRefusal::CurrentProjectGitHostIntegrationInUse.into());
    }

    let moved = gradient_db::tasks::move_to_project(&db.web_db, task, new_project.id)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::Task))?
        .or_not_found(Resource::Task)?;

    audit
        .write(
            Action::TaskMoveToProject,
            EventOwner::task(new_project.id, moved.id),
            serde_json::json!({
                "task": moved.name,
                "from_project_id": project.id.to_string(),
                "to_project_id": new_project.id.to_string(),
            }),
        )
        .await;

    let res = BaseResponse {
        error: false,
        message: "Ownership transferred".to_string(),
    };

    Ok(Json(res))
}

async fn task_references_git_host_integration(db: &WebDb, task: TaskId) -> WebResult<bool> {
    let triggers = gradient_db::triggers::of_task(db, task).await?;
    if triggers
        .iter()
        .any(|t| super::triggers::trigger_integration_id(t.trigger_type, &t.config).is_some())
    {
        return Ok(true);
    }
    let actions = gradient_db::actions::of_task(db, task).await?;
    Ok(actions.into_iter().any(|action| {
        matches!(
            serde_json::from_value::<ActionConfig>(action.config),
            Ok(ActionConfig::GitHostStatusReport { .. } | ActionConfig::OpenPr { .. })
        )
    }))
}
