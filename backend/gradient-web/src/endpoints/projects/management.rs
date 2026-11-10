/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, caller_project_mask, has_permission, key_limited_project_mask};
use crate::api_spec::ProjectPath;
use crate::audit::{Audit, changed_fields};
use crate::authorization::MaybeApiKey;
use crate::endpoints::access::{TeamNameAndDisplayName, team_names};
use crate::endpoints::teams::grants::{apply_owner_team_grant_change_to_workers, grant_workers};
use crate::endpoints::teams::management::lookup_owner_team_id_and_check_user_is_member_or_admin;
use crate::error::{WebError, WebResult, require_create_permission};
use crate::helpers::{CreatedEntityResponse, ok_json, paginate};
use crate::json_body::JsonBody;
use crate::names::{ProjectDisplayName, ProjectName};
use crate::permissions::{Permission, PermissionMask, mask_to_vec};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{ProjectScope, Readable, RequiresUnmanaged, can};
use anyhow::Context;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::input::check_nix_restricted_evaluation_url_prefix;

use gradient_core::ServerState;
use gradient_db::projects::ProjectEvaluationProgressCounts;
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
    #[serde(default)]
    pub owner_team: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchProjectRequest {
    pub name: Option<ProjectName>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub hide_build_requests: Option<bool>,
    pub allow_exec_in_build_requests: Option<bool>,
    pub restrict_evaluation_fetches: Option<bool>,
    pub extra_allowed_uri_prefixes: Option<Vec<String>>,
    /// Omitted fields leave the owner team unchanged. A team name can set the owner team. JSON null can clear the owner team.
    #[serde(
        default,
        deserialize_with = "crate::helpers::deserialize_optional_field"
    )]
    pub owner_team: Option<Option<String>>,
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
    pub allow_exec_in_build_requests: bool,
    pub restrict_evaluation_fetches: bool,
    pub extra_allowed_uri_prefixes: Vec<String>,
    pub managed: bool,
    pub owner_team: Option<TeamNameAndDisplayName>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub running_evaluations: i64,
    pub waiting_evaluations: i64,
    pub role: Option<String>,
    pub permissions: Vec<Permission>,
}

impl ProjectSummary {
    fn new(
        project: MProject,
        owner_team: Option<TeamNameAndDisplayName>,
        evaluations: ProjectEvaluationProgressCounts,
        role: Option<String>,
        permissions: PermissionMask,
    ) -> Self {
        Self {
            owner_team,
            id: project.id,
            name: project.name,
            display_name: project.display_name,
            description: project.description,
            public_key: Some(project.public_key),
            public: project.public,
            hide_build_requests: project.hide_build_requests,
            allow_exec_in_build_requests: project.allow_exec_in_build_requests,
            restrict_evaluation_fetches: project.restrict_evaluation_fetches,
            extra_allowed_uri_prefixes: project.extra_allowed_uri_prefixes,
            managed: project.managed,
            created_by: project.created_by,
            created_at: project.created_at,
            running_evaluations: evaluations.running,
            waiting_evaluations: evaluations.waiting,
            role,
            permissions: mask_to_vec(permissions),
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PublicProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub created_at: chrono::NaiveDateTime,
}

impl From<MProject> for PublicProjectSummary {
    fn from(project: MProject) -> Self {
        Self {
            id: project.id,
            name: project.name,
            display_name: project.display_name,
            description: project.description,
            created_at: project.created_at,
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
    pub allow_exec_in_build_requests: bool,
    pub restrict_evaluation_fetches: bool,
    pub extra_allowed_uri_prefixes: Vec<String>,
    pub managed: bool,
    pub owner_team: Option<TeamNameAndDisplayName>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub github_app_available: bool,
    pub github_app_install_url: Option<String>,
    pub role: Option<String>,
    pub permissions: Vec<Permission>,
}

impl ProjectResponse {
    fn new(
        project: MProject,
        owner_team: Option<TeamNameAndDisplayName>,
        role: Option<String>,
        permissions: PermissionMask,
        github_app_available: bool,
        github_app_install_url: Option<String>,
    ) -> Self {
        Self {
            owner_team,
            id: project.id,
            name: project.name,
            display_name: project.display_name,
            description: project.description,
            public_key: Some(project.public_key),
            public: project.public,
            hide_build_requests: project.hide_build_requests,
            allow_exec_in_build_requests: project.allow_exec_in_build_requests,
            restrict_evaluation_fetches: project.restrict_evaluation_fetches,
            extra_allowed_uri_prefixes: project.extra_allowed_uri_prefixes,
            managed: project.managed,
            created_by: project.created_by,
            created_at: project.created_at,
            github_app_available,
            github_app_install_url,
            role,
            permissions: mask_to_vec(permissions),
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
    description = "List all projects where the current user is a member. Response fields include the role of the caller and `permissions`. `running_evaluations` is the number of tasks whose latest evaluation is in the running state. `waiting_evaluations` is the number of tasks whose latest evaluation is in the `waiting` state. An API key for one project can list only that project.",
    params(PaginationParams),
    responses((status = OK, description = "Project list", body = PaginatedProjectSummary), WebError)
)]
pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(MaybeApiKey(api_key)): Extension<MaybeApiKey>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<ProjectSummary>>>> {
    let reachable = gradient_db::projects::reachable_by_user(
        user.id,
        api_key.as_ref().and_then(|key| key.project),
    );
    let listing = paginate(reachable, &state.web_db, &params).await?;

    let project_ids: Vec<ProjectId> = listing.items.iter().map(|o| o.id).collect();
    let evaluations_per_project =
        gradient_db::projects::get_running_and_waiting_evaluation_counts_by_project_id(
            &state.web_db,
            &project_ids,
        )
        .await?;

    let mut grants =
        gradient_db::access::project_grants(&state.web_db, user.id, project_ids).await?;
    let owner_teams = team_names(
        &state.web_db,
        listing
            .items
            .iter()
            .filter_map(|project| project.owner_team),
    )
    .await?;

    let listing = listing.map(|project| {
        let owner_team = project
            .owner_team
            .and_then(|team| owner_teams.get(&team).cloned());
        let evaluations = evaluations_per_project
            .get(&project.id)
            .copied()
            .unwrap_or_default();
        let (role, permissions) = match grants.remove(&project.id) {
            Some(grant) => (
                Some(grant.role),
                key_limited_project_mask(grant.permissions, project.id, api_key.as_ref()),
            ),
            None => (None, 0),
        };
        ProjectSummary::new(project, owner_team, evaluations, role, permissions)
    });

    Ok(ok_json(listing))
}

#[utoipa::path(
    put,
    path = "/projects",
    operation_id = "createProject",
    tag = "projects",
    summary = "Create project",
    description = "Create a new project for the current user. The response fields list the project UUID, the project name, and the URL of the web page for the project. A signed-in session can call this endpoint. An API key with `createProject` and no project pinning can also call this endpoint. Use the `owner_team` field to name a team with the caller as a member. Place the project in that team. Members of that team receive the default role for members of team projects.",
    request_body = MakeProjectRequest,
    responses((status = OK, description = "Project created", body = CreatedEntityResponse), WebError)
)]
pub async fn put(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Extension(MaybeApiKey(api_key)): Extension<MaybeApiKey>,
    JsonBody(body): JsonBody<MakeProjectRequest>,
) -> WebResult<Json<BaseResponse<CreatedEntityResponse>>> {
    if let Some(key) = api_key.as_ref()
        && !key.may_create_projects()
    {
        return Err(SharedRefusal::MissingPermission.into());
    }
    require_create_permission(state.config.permissions.create_project, &user)?;

    let existing_project =
        gradient_db::projects::by_name(&state.web_db, body.name.as_str()).await?;

    if existing_project.is_some() {
        return Err(WebError::already_exists(Resource::ProjectName));
    }

    let owner_team = match body.owner_team.as_deref() {
        Some(name) => Some(
            lookup_owner_team_id_and_check_user_is_member_or_admin(&state.web_db, &user, name)
                .await?,
        ),
        None => None,
    };

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
            owner_team,
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

    let owner_grant =
        gradient_db::teams::grants::upsert_project_owner_team_grant(&tx, project.id, owner_team)
            .await?;
    let worker_grants =
        gradient_db::teams::grants::insert_worker_grants_for_teams_that_grant_workers_on_every_new_project(
            &tx, project.id,
        )
        .await?;

    tx.commit().await?;

    audit
        .write(
            Action::ProjectCreate,
            EventOwner::project(project.id),
            serde_json::json!({ "project_id": project.id.to_string(), "name": project.name, }),
        )
        .await;

    // A connected worker is learning about the new project only when it re-auths.
    if let Err(e) =
        apply_owner_team_grant_change_to_workers(&state.web_db, &scheduler, project.id, owner_grant)
            .await
    {
        tracing::warn!(project = %project.id, error = %e, "Failed to read the workers of the team to request re-authentication for the new project");
    }
    for grant in worker_grants {
        if let Err(e) = grant_workers(&state.web_db, &scheduler, grant.team, project.id).await {
            tracing::warn!(team = %grant.team, error = %e, "Failed to read the workers of the team to request re-authentication for the new project");
        }
    }

    let url = frontend_page_urls::project(&state.config.server.frontend_url, &project.name);
    Ok(ok_json(CreatedEntityResponse::new(
        project.id,
        project.name,
        url,
    )))
}

type PaginatedPublicProjectSummary = Paginated<PublicProjectSummary>;

#[utoipa::path(
    get,
    path = "/projects/public",
    operation_id = "listPublicProjects",
    tag = "projects",
    summary = "List public projects",
    description = "Returns a paginated list of all public projects, ordered by name. Does not require authentication. Servers that run with `GRADIENT_PUBLIC_LISTING=false` return `404`.",
    params(PaginationParams),
    responses((status = OK, description = "Paginated public project list", body = PaginatedPublicProjectSummary), WebError)
)]
pub async fn get_public_projects(
    state: State<Arc<ServerState>>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<PublicProjectSummary>>>> {
    let listing = paginate(gradient_db::projects::all_public(), &state.web_db, &params).await?;

    Ok(ok_json(listing.map(PublicProjectSummary::from)))
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
        api_key,
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
    let permissions = caller_project_mask(
        &state.web_db,
        Caller::from(&maybe_user),
        project.id,
        api_key.as_ref(),
    )
    .await?;

    let install_url = github_app_install_url(
        &state.config,
        &state.http,
        &state.memory.github_app_install_url,
    )
    .await;
    let owner_team = team_names(&state.web_db, project.owner_team)
        .await?
        .into_values()
        .next();

    Ok(ok_json(ProjectResponse::new(
        project,
        owner_team,
        role,
        permissions,
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
    description = "Update one or more fields of a project. The API can't edit managed projects. API calls need an admin role for the project. Changes to `owner_team` also need the `manageMembers` permission. Callers need to be members of the new team.",
    params(ProjectPath),
    request_body = PatchProjectRequest,
    responses((status = OK, description = "Updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_project(
    ProjectScope {
        project,
        db,
        user,
        api_key,
        ..
    }: ProjectScope<RequiresUnmanaged<can::ManageProjectSettings>>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    JsonBody(body): JsonBody<PatchProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let owner_team = match &body.owner_team {
        None => None,
        Some(owner_team) => {
            if !has_permission(
                &db.web_db,
                user.id,
                project.id,
                Permission::ManageMembers,
                api_key.as_ref(),
            )
            .await?
            {
                return Err(SharedRefusal::MissingPermission.into());
            }
            Some(match owner_team.as_deref() {
                Some(name) => Some(
                    lookup_owner_team_id_and_check_user_is_member_or_admin(&db.web_db, &user, name)
                        .await?,
                ),
                None => None,
            })
        }
    };
    let fields = changed_fields([
        ("owner_team", owner_team.is_some()),
        ("name", body.name.is_some()),
        ("display_name", body.display_name.is_some()),
        ("description", body.description.is_some()),
        ("hide_build_requests", body.hide_build_requests.is_some()),
        (
            "allow_exec_in_build_requests",
            body.allow_exec_in_build_requests.is_some(),
        ),
        (
            "restrict_evaluation_fetches",
            body.restrict_evaluation_fetches.is_some(),
        ),
        (
            "extra_allowed_uri_prefixes",
            body.extra_allowed_uri_prefixes.is_some(),
        ),
    ]);
    let project_id = project.id;
    let mut aproject: AProject = project.into();

    if let Some(name) = body.name {
        let existing_project = gradient_db::projects::by_name(&db.web_db, name.as_str()).await?;

        if existing_project.is_some_and(|existing| existing.id != project_id) {
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
    crate::patch_field!(aproject, body, allow_exec_in_build_requests);
    crate::patch_field!(aproject, body, restrict_evaluation_fetches);

    if let Some(prefixes) = body.extra_allowed_uri_prefixes {
        let prefixes = prefixes
            .iter()
            .map(|prefix| check_nix_restricted_evaluation_url_prefix(prefix.trim()))
            .collect::<Result<Vec<_>, _>>()?;
        aproject.extra_allowed_uri_prefixes = Set(prefixes);
    }
    if let Some(owner_team) = owner_team {
        aproject.owner_team = Set(owner_team);
    }

    let tx = db.web_db.inner().begin().await?;
    let project = gradient_db::rows::update::<EProject, _>(&tx, aproject)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::ProjectName))?;
    let owner_grant = match owner_team {
        Some(owner_team) => Some(
            gradient_db::teams::grants::upsert_project_owner_team_grant(
                &tx, project.id, owner_team,
            )
            .await?,
        ),
        None => None,
    };
    tx.commit().await?;
    if let Some(owner_grant) = owner_grant {
        apply_owner_team_grant_change_to_workers(&db.web_db, &scheduler, project.id, owner_grant)
            .await?;
    }

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
