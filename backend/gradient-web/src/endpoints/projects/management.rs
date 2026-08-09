/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectAccess, load_project};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult, require_create_permission};
use crate::helpers::{ok_json, paginate, role_names};
use crate::permissions::Permission;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};

use gradient_core::ServerState;
use gradient_sources::generate_ssh_key;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::input::{check_index_name, validate_display_name};
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, JoinType, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeProjectRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public: Option<bool>,
    pub hide_build_requests: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchProjectRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub hide_build_requests: Option<bool>,
}

#[derive(Serialize)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub public: bool,
    pub hide_build_requests: bool,
    pub managed: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub running_evaluations: i64,
    pub role: Option<String>,
}

#[derive(Serialize)]
pub struct ProjectResponse {
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub public: bool,
    pub hide_build_requests: bool,
    pub managed: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    /// Whether the server has a GitHub App configured at all.
    pub github_app_available: bool,
    pub role: Option<String>,
}

pub async fn get_project_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(ok_json(false));
    }
    let exists = EProject::find()
        .filter(CProject::Name.eq(name.as_str()))
        .one(&state.web_db)
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

/// Count in-progress evaluations per project for `project_ids`.
///
/// Returns a map of project_id → count of evaluations in any active status
/// (Queued, Fetching, EvaluatingFlake, EvaluatingDerivation, Building, Waiting).
async fn count_running_evaluations(
    state: &Arc<ServerState>,
    project_ids: &[ProjectId],
) -> WebResult<HashMap<ProjectId, i64>> {
    use gradient_entity::evaluation::EvaluationStatus;

    if project_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let tasks = ETask::find()
        .filter(CTask::Project.is_in(project_ids.to_vec()))
        .all(&state.web_db)
        .await?;

    let task_ids: Vec<TaskId> = tasks.iter().map(|p| p.id).collect();
    let task_to_project: HashMap<TaskId, ProjectId> =
        tasks.into_iter().map(|p| (p.id, p.project)).collect();

    let mut running_per_project: HashMap<ProjectId, i64> = HashMap::new();
    if !task_ids.is_empty() {
        let running = EEvaluation::find()
            .filter(CEvaluation::Task.is_in(task_ids))
            .filter(CEvaluation::Status.is_in(EvaluationStatus::ACTIVE))
            .all(&state.web_db)
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

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<ProjectSummary>>>>> {
    let listing = paginate(
        EProject::find()
            .join_rev(
                JoinType::InnerJoin,
                EProjectUser::belongs_to(gradient_entity::project::Entity)
                    .from(CProjectUser::Project)
                    .to(CProject::Id)
                    .into(),
            )
            .filter(CProjectUser::User.eq(user.id))
            .order_by_asc(CProject::CreatedAt),
        &state.web_db,
        &params,
    )
    .await?;

    let project_ids: Vec<ProjectId> = listing.items.iter().map(|o| o.id).collect();
    let running_per_project = count_running_evaluations(&state, &project_ids).await?;

    let project_users = EProjectUser::find()
        .filter(CProjectUser::User.eq(user.id))
        .filter(CProjectUser::Project.is_in(project_ids))
        .all(&state.web_db)
        .await?;

    let role_name_map = role_names(
        &state.web_db,
        project_users.iter().map(|ou| ou.role).collect(),
    )
    .await?;
    let project_role_map: HashMap<ProjectId, String> = project_users
        .into_iter()
        .filter_map(|ou| role_name_map.get(&ou.role).map(|n| (ou.project, n.clone())))
        .collect();

    let listing = listing.map(|o| ProjectSummary {
        running_evaluations: *running_per_project.get(&o.id).unwrap_or(&0),
        role: project_role_map.get(&o.id).cloned(),
        id: o.id,
        name: o.name,
        display_name: o.display_name,
        description: o.description,
        public_key: Some(o.public_key),
        public: o.public,
        hide_build_requests: o.hide_build_requests,
        managed: o.managed,
        created_by: o.created_by,
        created_at: o.created_at,
    });

    Ok(ok_json(listing))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_create_permission(state.config.server.create_project, &user)?;

    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Project Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::bad_request(format!(
            "Invalid display name: {}",
            e
        )));
    }

    let existing_project = EProject::find()
        .filter(CProject::Name.eq(body.name.clone()))
        .one(&state.web_db)
        .await?;

    if existing_project.is_some() {
        return Err(WebError::already_exists("Project Name"));
    }

    let (private_key, public_key) = generate_ssh_key(&state.config.secrets.crypt_secret_file)
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to generate SSH key");
            WebError::failed_ssh_key_generation()
        })?;

    let tx = state.web_db.inner().begin().await?;

    let project = MProject {
        id: ProjectId::now_v7(),
        name: body.name.clone(),
        display_name: body.display_name.trim().to_string(),
        description: body.description.trim().to_string(),
        public_key,
        private_key,
        public: body.public.unwrap_or(false),
        hide_build_requests: body.hide_build_requests.unwrap_or(false),
        created_by: user.id,
        created_at: gradient_types::now(),
        ..Default::default()
    }
    .into_active_model()
    .insert(&tx)
    .await
    .map_err(|e| WebError::from_db_err(e, "Project Name"))?;

    MProjectUser {
        id: ProjectUserId::now_v7(),
        project: project.id,
        user: user.id,
        role: BASE_ROLE_ADMIN_ID,
    }
    .into_active_model()
    .insert(&tx)
    .await?;

    tx.commit().await?;

    Ok(Json(BaseResponse {
        error: false,
        message: project.id.to_string(),
    }))
}

pub async fn get_public_projects(
    state: State<Arc<ServerState>>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<MProject>>>>> {
    let listing = paginate(
        EProject::find()
            .filter(CProject::Public.eq(true))
            .order_by_asc(CProject::CreatedAt),
        &state.web_db,
        &params,
    )
    .await?;

    Ok(ok_json(listing))
}

pub async fn get_project(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<ProjectResponse>>> {
    let project = load_project(
        &state.0,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        ProjectAccess::Readable { label: "Project" },
    )
    .await?;

    let role = if let Some(ref user) = maybe_user {
        let project_user = EProjectUser::find()
            .filter(CProjectUser::User.eq(user.id))
            .filter(CProjectUser::Project.eq(project.id))
            .one(&state.web_db)
            .await?;
        if let Some(ou) = project_user {
            ERole::find_by_id(ou.role)
                .one(&state.web_db)
                .await?
                .map(|r| r.name)
        } else {
            None
        }
    } else {
        None
    };

    Ok(ok_json(ProjectResponse {
        id: project.id,
        name: project.name,
        display_name: project.display_name,
        description: project.description,
        public_key: Some(project.public_key),
        public: project.public,
        hide_build_requests: project.hide_build_requests,
        managed: project.managed,
        created_by: project.created_by,
        created_at: project.created_at,
        github_app_available: state.config.github_app.clone().is_some(),
        role,
    }))
}

pub async fn patch_project(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Json(body): Json<PatchProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageProjectSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut aproject: AProject = project.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err(WebError::invalid_name("Project Name"));
        }

        let existing_project = EProject::find()
            .filter(CProject::Name.eq(name.clone()))
            .one(&state.web_db)
            .await?;

        if existing_project.is_some() {
            return Err(WebError::already_exists("Project Name"));
        }

        aproject.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::bad_request(format!(
                "Invalid display name: {}",
                e
            )));
        }
        aproject.display_name = Set(display_name);
    }

    crate::patch_field_with!(aproject, body, description, |s: String| s
        .trim()
        .to_string());

    crate::patch_field!(aproject, body, hide_build_requests);

    let project = aproject
        .update(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Project Name"))?;

    let res = BaseResponse {
        error: false,
        message: project.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_project(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::DeleteProject,
            reject_managed: true,
        },
    )
    .await?;
    let project_id = project.id;
    let project_name = project.name.clone();
    let aproject: AProject = project.into();
    aproject.delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::PROJECT_DELETE,
        &info,
        Some(serde_json::json!({
            "project_id": project_id.to_string(),
            "project_name": project_name,
        })),
    )
    .await;

    let res = BaseResponse {
        error: false,
        message: "Project deleted".to_string(),
    };

    Ok(Json(res))
}
