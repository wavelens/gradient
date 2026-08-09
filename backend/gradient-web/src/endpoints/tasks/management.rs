/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::TaskResponse;
use super::auto_attach;
use crate::access::{Caller, ProjectAccess, TaskAccess, has_permission, load_project, load_task};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, paginate};
use crate::permissions::Permission;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};

use gradient_core::ServerState;
use gradient_db::get_any_project_by_name;
use gradient_nix::RepositoryUrl;
use gradient_sources::check_task_updates;
use gradient_types::consts::*;
use gradient_types::input::{check_task_name, validate_display_name, vec_to_hex};
use gradient_types::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerType};
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeTaskRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    #[serde(default)]
    pub concurrency: Option<ConcurrencyPolicy>,
    #[serde(default)]
    pub sign_cache: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchTaskRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub wildcard: Option<String>,
    pub keep_evaluations: Option<i32>,
    pub concurrency: Option<ConcurrencyPolicy>,
    pub sign_cache: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TransferOwnershipRequest {
    pub project: String,
}

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
        .or_not_found("Project")?;
    let exists = ETask::find()
        .filter(CTask::Name.eq(name.as_str()))
        .filter(CTask::Project.eq(project.id))
        .one(&state.web_db)
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<TaskResponse>>>>> {
    let api_key_ref = api_key.as_ref();
    let project = load_project(
        &state.0,
        Caller::from_option(&maybe_user),
        api_key_ref,
        project,
        ProjectAccess::Readable { label: "Project" },
    )
    .await?;

    let (can_edit, can_trigger) = match &maybe_user {
        Some(user) => (
            has_permission(
                &state,
                user.id,
                project.id,
                Permission::EditTask,
                api_key_ref,
            )
            .await?,
            has_permission(
                &state,
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
        ETask::find()
            .filter(CTask::Project.eq(project.id))
            .order_by_asc(CTask::CreatedAt),
        &state.web_db,
        &params,
    )
    .await?;

    // Batch-fetch the status of the last evaluation for each task.
    let eval_ids: Vec<EvaluationId> = listing
        .items
        .iter()
        .filter_map(|p| p.last_evaluation)
        .collect();
    let db = &state.web_db;
    let eval_status_map: HashMap<EvaluationId, gradient_entity::evaluation::EvaluationStatus> =
        gradient_db::fetch_in_chunks(&eval_ids, |chunk| async move {
            EEvaluation::find()
                .filter(CEvaluation::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|e| (e.id, e.status))
        .collect();

    let listing = listing.map(|p| {
        let last_evaluation_status = p
            .last_evaluation
            .and_then(|id| eval_status_map.get(&id).cloned());
        TaskResponse {
            id: p.id,
            project: p.project,
            name: p.name,
            active: p.active,
            display_name: p.display_name,
            description: p.description,
            repository: p.repository,
            wildcard: p.wildcard,
            last_evaluation: p.last_evaluation,
            last_evaluation_status,
            force_evaluation: p.force_evaluation,
            keep_evaluations: p.keep_evaluations,
            concurrency: p.concurrency,
            created_by: p.created_by,
            created_at: p.created_at,
            managed: p.managed,
            sign_cache: p.sign_cache,
            can_edit,
            can_trigger,
        }
    });

    Ok(ok_json(listing))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Json(body): Json<MakeTaskRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if check_task_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Task Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::bad_request(format!(
            "Invalid display name: {}",
            e
        )));
    }

    body.repository
        .parse::<RepositoryUrl>()
        .map_err(|e| WebError::bad_request(e.to_string()))?;

    let project = load_project(
        &state.0,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::CreateTask,
            reject_managed: true,
        },
    )
    .await?;

    let existing_task = ETask::find()
        .filter(
            Condition::all()
                .add(CTask::Project.eq(project.id))
                .add(CTask::Name.eq(body.name.clone())),
        )
        .one(&state.web_db)
        .await?;

    if existing_task.is_some() {
        return Err(WebError::already_exists("Task Name"));
    }

    let wildcard = body
        .wildcard
        .trim()
        .parse::<Wildcard>()
        .map_err(|e| WebError::bad_request(e.to_string()))?
        .to_string();

    let task = MTask {
        id: TaskId::now_v7(),
        project: project.id,
        name: body.name.clone(),
        active: true,
        display_name: body.display_name.trim().to_string(),
        description: body.description.trim().to_string(),
        repository: body.repository.clone(),
        wildcard,
        last_check_at: *NULL_TIME,
        created_by: user.id,
        created_at: gradient_types::now(),
        keep_evaluations: state.config.storage.default_keep_evaluations(),
        concurrency: body.concurrency.unwrap_or(ConcurrencyPolicy::SoftAbort),
        sign_cache: body.sign_cache.unwrap_or(true),
        ..Default::default()
    }
    .into_active_model();

    let task = task.insert(&state.web_db).await?;

    let now = gradient_types::now();
    let default_cfg = TriggerConfig::Polling {
        interval_secs: 300,
        branch: None,
    };
    MTaskTrigger {
        id: TaskTriggerId::now_v7(),
        task: task.id,
        trigger_type: TriggerType::Polling,
        config: default_cfg.to_db_json(),
        active: true,
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    let integrations = EIntegration::find()
        .filter(CIntegration::Project.eq(project.id))
        .all(&state.web_db)
        .await
        .unwrap_or_default();
    if let Err(e) = auto_attach::apply(&state.web_db, &task, &integrations).await {
        tracing::warn!(
            "auto-attaching integrations to task {} failed: {e}",
            task.id
        );
    }

    let res = BaseResponse {
        error: false,
        message: task.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_task(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<TaskResponse>>> {
    let api_key_ref = api_key.as_ref();
    let (project, task) = load_task(
        &state.0,
        Caller::from_option(&maybe_user),
        api_key_ref,
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;

    let (can_edit, can_trigger) = match &maybe_user {
        Some(user) => (
            has_permission(
                &state,
                user.id,
                project.id,
                Permission::EditTask,
                api_key_ref,
            )
            .await?,
            has_permission(
                &state,
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
        EEvaluation::find_by_id(eval_id)
            .one(&state.web_db)
            .await?
            .map(|e| e.status)
    } else {
        None
    };

    Ok(ok_json(TaskResponse {
        id: task.id,
        project: task.project,
        name: task.name,
        active: task.active,
        display_name: task.display_name,
        description: task.description,
        repository: task.repository,
        wildcard: task.wildcard,
        last_evaluation: task.last_evaluation,
        last_evaluation_status,
        force_evaluation: task.force_evaluation,
        created_by: task.created_by,
        created_at: task.created_at,
        managed: task.managed,
        keep_evaluations: task.keep_evaluations,
        concurrency: task.concurrency,
        sign_cache: task.sign_cache,
        can_edit,
        can_trigger,
    }))
}

pub async fn patch_task(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Json(body): Json<PatchTaskRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (project, task) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::EditTask,
            reject_managed: true,
        },
    )
    .await?;
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

    atask.force_evaluation = Set(true);
    atask.update(&state.web_db).await?;

    Ok(ok_json("Task updated".to_string()))
}

/// Holds shared context for the task-patch field validators so that
/// `state` and `atask` are not threaded through every helper as parameters.
struct TaskPatcher<'a> {
    state: &'a State<Arc<ServerState>>,
    atask: &'a mut ATask,
}

impl<'a> TaskPatcher<'a> {
    fn new(state: &'a State<Arc<ServerState>>, atask: &'a mut ATask) -> Self {
        Self { state, atask }
    }

    async fn apply_name(&mut self, project: &MProject, name: String) -> WebResult<()> {
        if check_task_name(name.as_str()).is_err() {
            return Err(WebError::invalid_name("Task Name"));
        }
        let existing = ETask::find()
            .filter(
                Condition::all()
                    .add(CTask::Project.eq(project.id))
                    .add(CTask::Name.eq(name.clone())),
            )
            .one(&self.state.web_db)
            .await?;
        if existing.is_some() {
            return Err(WebError::already_exists("Task Name"));
        }
        self.atask.name = Set(name);
        Ok(())
    }

    fn apply_display_name(&mut self, display_name: String) -> WebResult<()> {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::bad_request(format!(
                "Invalid display name: {}",
                e
            )));
        }
        self.atask.display_name = Set(display_name);
        Ok(())
    }

    fn apply_repository(&mut self, repository: String) -> WebResult<()> {
        repository
            .parse::<RepositoryUrl>()
            .map_err(|e| WebError::bad_request(e.to_string()))?;
        self.atask.repository = Set(repository);
        Ok(())
    }

    fn apply_wildcard(&mut self, wildcard: String) -> WebResult<()> {
        let wildcard = wildcard
            .trim()
            .parse::<Wildcard>()
            .map_err(|e| WebError::bad_request(e.to_string()))?
            .to_string();
        self.atask.wildcard = Set(wildcard);
        Ok(())
    }

    fn apply_keep_evaluations(&mut self, keep: i32) -> WebResult<()> {
        if keep < 1 {
            return Err(WebError::bad_request(
                "keep_evaluations must be at least 1".to_string(),
            ));
        }
        if let Some(global_max) = self.state.config.storage.keep_evaluations_max()
            && keep > global_max
        {
            return Err(WebError::bad_request(format!(
                "keep_evaluations cannot exceed the server maximum of {}",
                global_max
            )));
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

pub async fn delete_task(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (project_row, task) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::EditTask,
            reject_managed: true,
        },
    )
    .await?;
    let task_id = task.id;
    let task_name = task.name.clone();
    let atask: ATask = task.into();
    atask.delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::TASK_DELETE,
        &info,
        Some(serde_json::json!({
            "project_id": project_row.id.to_string(),
            "task_id": task_id.to_string(),
            "task_name": task_name,
        })),
    )
    .await;

    let res = BaseResponse {
        error: false,
        message: "Task deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_task_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_project, task) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::EditTask,
            reject_managed: true,
        },
    )
    .await?;
    let mut atask: ATask = task.into();
    atask.active = Set(true);
    atask.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "Task enabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_task_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_project, task) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::EditTask,
            reject_managed: true,
        },
    )
    .await?;
    let mut atask: ATask = task.into();
    atask.active = Set(false);
    atask.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "Task disabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_task_check_repository(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_project, task) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::EditTask,
            reject_managed: true,
        },
    )
    .await?;

    let (_has_updates, remote_hash) =
        check_task_updates(&state.db(), &task, None)
            .await
            .map_err(|e| {
                WebError::bad_request_with(
                    ErrorCode::REPOSITORY_UNREACHABLE,
                    format!("Failed to check repository: {}", e),
                )
            })?;

    let res = BaseResponse {
        error: false,
        message: vec_to_hex(&remote_hash),
    };

    Ok(Json(res))
}

pub async fn post_task_transfer(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Json(body): Json<TransferOwnershipRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key_ref = api_key.as_ref();
    let (project, task) = load_task(
        &state,
        Caller::User(&user),
        api_key_ref,
        project,
        task,
        TaskAccess::Member,
    )
    .await?;

    // Only a project member with EditTask permission, or the current owner,
    // may transfer ownership.
    let is_admin = has_permission(
        &state,
        user.id,
        project.id,
        Permission::EditTask,
        api_key_ref,
    )
    .await?;
    let is_owner = task.created_by == user.id;
    if !is_admin && !is_owner {
        return Err(WebError::forbidden(
            "Only the task owner or a project admin can transfer ownership.".to_string(),
        ));
    }

    if task.managed {
        return Err(WebError::forbidden(
            "Cannot transfer ownership of a state-managed task.".to_string(),
        ));
    }

    let new_project = load_project(
        &state.0,
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
        return Err(WebError::bad_request(
            "Task is already in this project.".to_string(),
        ));
    }

    let mut atask: ATask = task.into();
    atask.project = Set(new_project.id);
    atask.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "Ownership transferred".to_string(),
    };

    Ok(Json(res))
}
