/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ProjectResponse;
use crate::access::{Caller, OrgAccess, ProjectAccess, has_permission, load_org, load_project};
use crate::helpers::{OptionExt, ok_json};
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::permissions::Permission;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};

use gradient_core::db::get_any_organization_by_name;
use gradient_core::nix::RepositoryUrl;
use gradient_core::sources::check_project_updates;
use gradient_core::types::consts::*;
use gradient_core::types::input::{check_index_name, validate_display_name, vec_to_hex};
use gradient_core::types::wildcard::Wildcard;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeProjectRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchProjectRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub evaluation_wildcard: Option<String>,
    pub keep_evaluations: Option<i32>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TransferOwnershipRequest {
    pub organization: String,
}

pub async fn get_project_name_available(
    state: State<Arc<ServerState>>,
    Path(organization): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(ok_json(false));
    }
    let org = get_any_organization_by_name(state.0.clone(), organization)
        .await?
        .or_not_found("Organization")?;
    let exists = EProject::find()
        .filter(CProject::Name.eq(name.as_str()))
        .filter(CProject::Organization.eq(org.id))
        .one(&state.web_db)
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(organization): Path<String>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<ProjectResponse>>>>> {
    let organization = load_org(
        &state.0,
        Caller::from_option(&maybe_user),
        organization,
        OrgAccess::Readable { label: "Organization" },
    )
    .await?;

    let page = params.page();
    let per_page = params.per_page();
    let can_edit = match &maybe_user {
        Some(user) => has_permission(&state, user.id, organization.id, Permission::EditProject).await?,
        None => false,
    };

    let paginator = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .order_by_asc(CProject::CreatedAt)
        .paginate(&state.web_db, per_page);

    let total = paginator.num_items().await?;
    let raw = paginator.fetch_page(page - 1).await?;

    // Batch-fetch the status of the last evaluation for each project.
    let eval_ids: Vec<Uuid> = raw.iter().filter_map(|p| p.last_evaluation).collect();
    let eval_status_map: HashMap<Uuid, entity::evaluation::EvaluationStatus> =
        if eval_ids.is_empty() {
            HashMap::new()
        } else {
            EEvaluation::find()
                .filter(CEvaluation::Id.is_in(eval_ids))
                .all(&state.web_db)
                .await?
                .into_iter()
                .map(|e| (e.id, e.status))
                .collect()
        };

    let items: Vec<ProjectResponse> = raw
        .into_iter()
        .map(|p| {
            let last_evaluation_status = p
                .last_evaluation
                .and_then(|id| eval_status_map.get(&id).cloned());
            ProjectResponse {
                id: p.id,
                organization: p.organization,
                name: p.name,
                active: p.active,
                display_name: p.display_name,
                description: p.description,
                repository: p.repository,
                evaluation_wildcard: p.evaluation_wildcard,
                last_evaluation: p.last_evaluation,
                last_evaluation_status,
                force_evaluation: p.force_evaluation,
                keep_evaluations: p.keep_evaluations,
                created_by: p.created_by,
                created_at: p.created_at,
                managed: p.managed,
                can_edit,
            }
        })
        .collect();

    Ok(ok_json(Paginated {
            items,
            total,
            page,
            per_page,
        }))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<MakeProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Project Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::bad_request(format!("Invalid display name: {}", e)));
    }

    body.repository
        .parse::<RepositoryUrl>()
        .map_err(|e| WebError::BadRequest(e.to_string()))?;

    let organization = load_org(
        &state.0,
        Caller::User(&user),
        organization,
        OrgAccess::Require {
            permission: Permission::CreateProject,
            reject_managed: true,
        },
    )
    .await?;

    let existing_project = EProject::find()
        .filter(
            Condition::all()
                .add(CProject::Organization.eq(organization.id))
                .add(CProject::Name.eq(body.name.clone())),
        )
        .one(&state.web_db)
        .await?;

    if existing_project.is_some() {
        return Err(WebError::already_exists("Project Name"));
    }

    let evaluation_wildcard = body
        .evaluation_wildcard
        .trim()
        .parse::<Wildcard>()
        .map_err(|e| WebError::BadRequest(e.to_string()))?
        .to_string();

    let project = AProject {
        id: Set(Uuid::now_v7()),
        organization: Set(organization.id),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.trim().to_string()),
        description: Set(body.description.trim().to_string()),
        repository: Set(body.repository.clone()),
        evaluation_wildcard: Set(evaluation_wildcard),
        last_evaluation: Set(None),
        last_check_at: Set(*NULL_TIME),
        force_evaluation: Set(false),
        created_by: Set(user.id),
        created_at: Set(gradient_core::types::now()),
        managed: Set(false),
        keep_evaluations: Set(30),
    };

    let project = project.insert(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: project.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_project(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectResponse>>> {
    let (organization, project) = load_project(
        &state.0,
        Caller::from_option(&maybe_user),
        organization,
        project,
        ProjectAccess::Readable,
    )
    .await?;

    let can_edit = match &maybe_user {
        Some(user) => has_permission(&state, user.id, organization.id, Permission::EditProject).await?,
        None => false,
    };

    let last_evaluation_status = if let Some(eval_id) = project.last_evaluation {
        EEvaluation::find_by_id(eval_id)
            .one(&state.web_db)
            .await?
            .map(|e| e.status)
    } else {
        None
    };

    Ok(ok_json(ProjectResponse {
            id: project.id,
            organization: project.organization,
            name: project.name,
            active: project.active,
            display_name: project.display_name,
            description: project.description,
            repository: project.repository,
            evaluation_wildcard: project.evaluation_wildcard,
            last_evaluation: project.last_evaluation,
            last_evaluation_status,
            force_evaluation: project.force_evaluation,
            created_by: project.created_by,
            created_at: project.created_at,
            managed: project.managed,
            keep_evaluations: project.keep_evaluations,
            can_edit,
        }))
}

pub async fn patch_project(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<PatchProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (organization, project) =
        load_project(&state, Caller::User(&user), organization, project, ProjectAccess::Require { permission: Permission::EditProject, reject_managed: true }).await?;
    let mut aproject: AProject = project.into();
    let mut patcher = ProjectPatcher::new(&state, &mut aproject);

    if let Some(name) = body.name {
        patcher.apply_name(&organization, name).await?;
    }
    if let Some(display_name) = body.display_name {
        patcher.apply_display_name(display_name)?;
    }
    if let Some(description) = body.description {
        patcher.aproject.description = Set(description.trim().to_string());
    }
    if let Some(repository) = body.repository {
        patcher.apply_repository(repository)?;
    }
    if let Some(evaluation_wildcard) = body.evaluation_wildcard {
        patcher.apply_evaluation_wildcard(evaluation_wildcard)?;
    }
    if let Some(keep) = body.keep_evaluations {
        patcher.apply_keep_evaluations(keep)?;
    }

    aproject.force_evaluation = Set(true);
    aproject.update(&state.web_db).await?;

    Ok(ok_json("Project updated".to_string()))
}

/// Holds shared context for the project-patch field validators so that
/// `state` and `aproject` are not threaded through every helper as parameters.
struct ProjectPatcher<'a> {
    state: &'a State<Arc<ServerState>>,
    aproject: &'a mut AProject,
}

impl<'a> ProjectPatcher<'a> {
    fn new(state: &'a State<Arc<ServerState>>, aproject: &'a mut AProject) -> Self {
        Self { state, aproject }
    }

    async fn apply_name(&mut self, organization: &MOrganization, name: String) -> WebResult<()> {
        if check_index_name(name.as_str()).is_err() {
            return Err(WebError::invalid_name("Project Name"));
        }
        let existing = EProject::find()
            .filter(
                Condition::all()
                    .add(CProject::Organization.eq(organization.id))
                    .add(CProject::Name.eq(name.clone())),
            )
            .one(&self.state.web_db)
            .await?;
        if existing.is_some() {
            return Err(WebError::already_exists("Project Name"));
        }
        self.aproject.name = Set(name);
        Ok(())
    }

    fn apply_display_name(&mut self, display_name: String) -> WebResult<()> {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::bad_request(format!("Invalid display name: {}", e)));
        }
        self.aproject.display_name = Set(display_name);
        Ok(())
    }

    fn apply_repository(&mut self, repository: String) -> WebResult<()> {
        repository
            .parse::<RepositoryUrl>()
            .map_err(|e| WebError::BadRequest(e.to_string()))?;
        self.aproject.repository = Set(repository);
        Ok(())
    }

    fn apply_evaluation_wildcard(&mut self, evaluation_wildcard: String) -> WebResult<()> {
        let evaluation_wildcard = evaluation_wildcard
            .trim()
            .parse::<Wildcard>()
            .map_err(|e| WebError::BadRequest(e.to_string()))?
            .to_string();
        self.aproject.evaluation_wildcard = Set(evaluation_wildcard);
        Ok(())
    }

    fn apply_keep_evaluations(&mut self, keep: i32) -> WebResult<()> {
        if keep < 1 {
            return Err(WebError::BadRequest(
                "keep_evaluations must be at least 1".to_string(),
            ));
        }
        let global_max = self.state.config.storage.keep_evaluations as i32;
        if global_max > 0 && keep > global_max {
            return Err(WebError::bad_request(format!(
                "keep_evaluations cannot exceed the server maximum of {}",
                global_max
            )));
        }
        self.aproject.keep_evaluations = Set(keep);
        Ok(())
    }
}

pub async fn delete_project(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_organization, project) =
        load_project(&state, Caller::User(&user), organization, project, ProjectAccess::Require { permission: Permission::EditProject, reject_managed: true }).await?;
    let aproject: AProject = project.into();
    aproject.delete(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "Project deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_project_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_organization, project) =
        load_project(&state, Caller::User(&user), organization, project, ProjectAccess::Require { permission: Permission::EditProject, reject_managed: true }).await?;
    let mut aproject: AProject = project.into();
    aproject.active = Set(true);
    aproject.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "Project enabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_project_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_organization, project) =
        load_project(&state, Caller::User(&user), organization, project, ProjectAccess::Require { permission: Permission::EditProject, reject_managed: true }).await?;
    let mut aproject: AProject = project.into();
    aproject.active = Set(false);
    aproject.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "Project disabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_project_check_repository(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_organization, project) =
        load_project(&state, Caller::User(&user), organization, project, ProjectAccess::Require { permission: Permission::EditProject, reject_managed: true }).await?;

    let (_has_updates, remote_hash) = check_project_updates(Arc::clone(&state), &project)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    if !remote_hash.is_empty() {
        let res = BaseResponse {
            error: false,
            message: vec_to_hex(&remote_hash),
        };

        Ok(Json(res))
    } else {
        Err(WebError::InternalServerError(
            "Failed to check repository".to_string(),
        ))
    }
}

pub async fn post_project_transfer(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<TransferOwnershipRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (organization, project) = load_project(
        &state,
        Caller::User(&user),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    // Only an org member with EditProject permission, or the current owner,
    // may transfer ownership.
    let is_admin = has_permission(&state, user.id, organization.id, Permission::EditProject).await?;
    let is_owner = project.created_by == user.id;
    if !is_admin && !is_owner {
        return Err(WebError::Forbidden(
            "Only the project owner or an organization admin can transfer ownership.".to_string(),
        ));
    }

    if project.managed {
        return Err(WebError::Forbidden(
            "Cannot transfer ownership of a state-managed project.".to_string(),
        ));
    }

    let new_organization = load_org(
        &state.0,
        Caller::User(&user),
        body.organization.clone(),
        OrgAccess::Require {
            permission: Permission::CreateProject,
            reject_managed: true,
        },
    )
    .await?;

    if new_organization.id == organization.id {
        return Err(WebError::BadRequest(
            "Project is already in this organization.".to_string(),
        ));
    }

    let mut aproject: AProject = project.into();
    aproject.organization = Set(new_organization.id);
    aproject.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "Ownership transferred".to_string(),
    };

    Ok(Json(res))
}
