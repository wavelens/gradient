/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::ProjectPath;
use crate::audit::{Audit, changed_fields};
use crate::error::{WebError, WebResult, require_create_permission};
use crate::helpers::{ok_json, paginate};
use crate::json_body::JsonBody;
use crate::names::{ProjectDisplayName, ProjectName};
use crate::resource::Resource;
use crate::scope::{ProjectScope, Readable, RequiresUnmanaged, can};
use anyhow::Context;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;

use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_sources::generate_ssh_key;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeProjectRequest {
    pub name: ProjectName,
    pub display_name: ProjectDisplayName,
    pub description: String,
    pub public: Option<bool>,
    pub hide_build_requests: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchProjectRequest {
    pub name: Option<ProjectName>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub hide_build_requests: Option<bool>,
    pub exec_enabled: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
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

impl ProjectSummary {
    fn new(project: MProject, running_evaluations: i64, role: Option<String>) -> Self {
        Self {
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
            running_evaluations,
            role,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectResponse {
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub public: bool,
    pub hide_build_requests: bool,
    pub exec_enabled: bool,
    pub managed: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub github_app_available: bool,
    pub github_app_install_url: Option<String>,
    pub role: Option<String>,
}

impl ProjectResponse {
    fn new(
        project: MProject,
        role: Option<String>,
        github_app_available: bool,
        github_app_install_url: Option<String>,
    ) -> Self {
        Self {
            id: project.id,
            name: project.name,
            display_name: project.display_name,
            description: project.description,
            public_key: Some(project.public_key),
            public: project.public,
            hide_build_requests: project.hide_build_requests,
            exec_enabled: project.exec_enabled,
            managed: project.managed,
            created_by: project.created_by,
            created_at: project.created_at,
            github_app_available,
            github_app_install_url,
            role,
        }
    }
}

#[utoipa::path(
    get,
    path = "/projects/available",
    operation_id = "checkProjectNameAvailable",
    tag = "projects",
    summary = "Check project name availability",
    description = "Returns `true` if no project with the given name exists, `false` if it is already taken.",
    params(("name" = String, Query)),
    responses((status = OK, description = "Availability result", body = bool, content_type = "application/json"), WebError)
)]
pub async fn get_project_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    let Ok(name) = ProjectName::try_from(name) else {
        return Ok(ok_json(false));
    };
    let exists = gradient_db::projects::by_name(&state.web_db, name.as_str())
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

type PaginatedProjectSummary = Paginated<ProjectSummary>;

#[utoipa::path(
    get,
    path = "/projects",
    operation_id = "listProjects",
    tag = "projects",
    summary = "List projects",
    description = "Returns all projects the current user is a member of.",
    params(PaginationParams),
    responses((status = OK, description = "Project list", body = PaginatedProjectSummary), WebError)
)]
pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<ProjectSummary>>>> {
    let listing = paginate(
        gradient_db::projects::reachable_by_user(user.id),
        &state.web_db,
        &params,
    )
    .await?;

    let project_ids: Vec<ProjectId> = listing.items.iter().map(|o| o.id).collect();
    let running_per_project =
        gradient_db::projects::running_evaluations_per_project(&state.web_db, &project_ids).await?;

    let project_role_map =
        gradient_db::access::project_role_names(&state.web_db, user.id, project_ids).await?;

    let listing = listing.map(|project| {
        let running_evaluations = *running_per_project.get(&project.id).unwrap_or(&0);
        let role = project_role_map.get(&project.id).cloned();
        ProjectSummary::new(project, running_evaluations, role)
    });

    Ok(ok_json(listing))
}

#[utoipa::path(
    put,
    path = "/projects",
    operation_id = "createProject",
    tag = "projects",
    summary = "Create project",
    description = "Creates a new project owned by the current user.",
    request_body = MakeProjectRequest,
    responses((status = OK, description = "Project UUID", body = String, content_type = "application/json"), WebError)
)]
pub async fn put(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    JsonBody(body): JsonBody<MakeProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_create_permission(state.config.permissions.create_project, &user)?;

    let existing_project =
        gradient_db::projects::by_name(&state.web_db, body.name.as_str()).await?;

    if existing_project.is_some() {
        return Err(WebError::already_exists(Resource::ProjectName));
    }

    let (private_key, public_key) =
        generate_ssh_key(&state.config.secrets.crypt_file).map_err(|e| {
            tracing::error!(error = %e, "Failed to generate SSH key");
            WebError::failed_ssh_key_generation()
        })?;

    let tx = state.web_db.inner().begin().await?;

    let project = gradient_db::rows::insert::<EProject, _>(
        &tx,
        MProject {
            id: ProjectId::now_v7(),
            name: body.name.to_string(),
            display_name: body.display_name.trim().to_string(),
            description: body.description.trim().to_string(),
            public_key,
            private_key,
            public: body.public.unwrap_or(false),
            hide_build_requests: body.hide_build_requests.unwrap_or(false),
            created_by: user.id,
            created_at: gradient_types::now(),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::ProjectName))?;

    gradient_db::rows::insert::<EProjectUser, _>(
        &tx,
        MProjectUser {
            id: ProjectUserId::now_v7(),
            project: project.id,
            user: user.id,
            role: BASE_ROLE_ADMIN_ID,
        },
    )
    .await?;

    let granted_teams =
        gradient_db::teams::grants::apply_new_project_grants(&tx, project.id).await?;

    tx.commit().await?;

    audit
        .write(
            Action::ProjectCreate,
            EventOwner::project(project.id),
            serde_json::json!({ "project_id": project.id.to_string(), "name": project.name, }),
        )
        .await;

    // A connected worker is learning about the new project only when it re-auths.
    for team in granted_teams {
        let workers = gradient_db::teams::team_worker_ids(&state.web_db, team).await?;
        crate::endpoints::teams::workers::reauth_team_workers(&scheduler, &workers).await;
    }

    Ok(Json(BaseResponse {
        error: false,
        message: project.id.to_string(),
    }))
}

type PaginatedProject = Paginated<MProject>;

#[utoipa::path(
    get,
    path = "/projects/public",
    operation_id = "listPublicProjects",
    tag = "projects",
    summary = "List public projects",
    description = "Returns a paginated list of all public projects, ordered by name. Does not require authentication.",
    params(PaginationParams),
    responses((status = OK, description = "Paginated public project list", body = PaginatedProject), WebError)
)]
pub async fn get_public_projects(
    state: State<Arc<ServerState>>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<MProject>>>> {
    let listing = paginate(gradient_db::projects::all_public(), &state.web_db, &params).await?;

    Ok(ok_json(listing))
}

#[utoipa::path(
    get,
    path = "/projects/{project}",
    operation_id = "getProject",
    tag = "projects",
    summary = "Get project",
    params(ProjectPath),
    responses((status = OK, description = "Project details", body = ProjectResponse), WebError)
)]
pub async fn get_project(
    ProjectScope {
        project,
        user: maybe_user,
        ..
    }: ProjectScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<ProjectResponse>>> {
    let role = match maybe_user {
        Some(ref user) => {
            gradient_db::access::project_role_names(&state.web_db, user.id, vec![project.id])
                .await?
                .remove(&project.id)
        }
        None => None,
    };

    let install_url = github_app_install_url(
        &state.config,
        &state.http,
        &state.memory.github_app_install_url,
    )
    .await;

    Ok(ok_json(ProjectResponse::new(
        project,
        role,
        state.config.github_app.is_some(),
        install_url,
    )))
}

const GITHUB_APP_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

async fn github_app_install_url(
    config: &RuntimeConfig,
    http: &reqwest::Client,
    github_app_install_url: &tokio::sync::OnceCell<String>,
) -> Option<String> {
    let app = config.github_app.as_ref()?;
    let lookup = async {
        let pem = tokio::fs::read_to_string(&app.private_key_file)
            .await
            .context("read GitHub App private key")?;
        gradient_git_host::github_app::get_install_url(http, app.app_id, &pem).await
    };
    github_app_install_url
        .get_or_try_init(|| async {
            tokio::time::timeout(GITHUB_APP_LOOKUP_TIMEOUT, lookup)
                .await
                .context("GitHub App lookup timed out")?
        })
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "GitHub App install URL lookup failed"))
        .ok()
        .cloned()
}

#[utoipa::path(
    patch,
    path = "/projects/{project}",
    operation_id = "patchProject",
    tag = "projects",
    summary = "Update project",
    description = "Updates one or more project fields. Managed projects cannot\nbe edited via the API. Requires the caller to hold the **admin** role\nin the project.",
    params(ProjectPath),
    request_body = PatchProjectRequest,
    responses((status = OK, description = "Updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_project(
    ProjectScope { project, db, .. }: ProjectScope<RequiresUnmanaged<can::ManageProjectSettings>>,
    audit: Audit,
    JsonBody(body): JsonBody<PatchProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let fields = changed_fields([
        ("name", body.name.is_some()),
        ("display_name", body.display_name.is_some()),
        ("description", body.description.is_some()),
        ("hide_build_requests", body.hide_build_requests.is_some()),
        ("exec_enabled", body.exec_enabled.is_some()),
    ]);
    let mut aproject: AProject = project.into();

    if let Some(name) = body.name {
        let existing_project = gradient_db::projects::by_name(&db.web_db, name.as_str()).await?;

        if existing_project.is_some() {
            return Err(WebError::already_exists(Resource::ProjectName));
        }

        aproject.name = Set(name.into_inner());
    }

    if let Some(display_name) = body.display_name {
        let display_name = ProjectDisplayName::try_from(display_name.trim().to_string())?;
        aproject.display_name = Set(display_name.into_inner());
    }

    crate::patch_field_with!(aproject, body, description, |s: String| s
        .trim()
        .to_string());

    crate::patch_field!(aproject, body, hide_build_requests);
    crate::patch_field!(aproject, body, exec_enabled);

    let project = gradient_db::rows::update::<EProject, _>(&db.web_db, aproject)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::ProjectName))?;

    audit
        .write(
            Action::ProjectUpdate,
            EventOwner::project(project.id),
            fields,
        )
        .await;

    let res = BaseResponse {
        error: false,
        message: project.id.to_string(),
    };

    Ok(Json(res))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}",
    operation_id = "deleteProject",
    tag = "projects",
    summary = "Delete project",
    description = "Permanently deletes the project and all its resources. Requires\nthe caller to hold the **admin** role in the project.",
    params(ProjectPath),
    responses((status = OK, description = "Deleted", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_project(
    ProjectScope { project, db, .. }: ProjectScope<RequiresUnmanaged<can::DeleteProject>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    let project_id = project.id;
    let project_name = project.name.clone();
    gradient_db::rows::delete::<EProject, _>(&db.web_db, project).await?;

    audit.write(Action::ProjectDelete, EventOwner::project(project_id), serde_json::json!({ "project_id": project_id.to_string(), "project_name": project_name, }))
    .await;

    let res = BaseResponse {
        error: false,
        message: "Project deleted".to_string(),
    };

    Ok(Json(res))
}
