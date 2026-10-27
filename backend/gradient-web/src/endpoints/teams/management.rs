/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TeamRefusal;
use super::workers::withdraw_team_workers;
use crate::api_spec::TeamPath;
use crate::audit::Audit;
use crate::error::{WebError, WebResult, require_create_permission, require_superuser};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::names::{TeamDisplayName, TeamName};
use crate::resource::Resource;
use crate::scope::{AdminUnmanaged, Member, TeamScope};
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Serialize, utoipa::ToSchema)]
pub struct TeamSummary {
    pub name: String,
    pub display_name: String,
    pub role: TeamRole,
}

impl TeamSummary {
    fn new(team: MTeam, role: TeamRole) -> Self {
        Self {
            name: team.name,
            display_name: team.display_name,
            role,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TeamResponse {
    pub id: TeamId,
    pub name: String,
    pub display_name: String,
    pub managed: bool,
    pub role: Option<TeamRole>,
    pub oidc_group: Option<String>,
    pub scim_group: Option<String>,
    pub new_project_users: bool,
    pub new_project_workers: bool,
    pub new_project_role: Option<String>,
}

impl TeamResponse {
    fn new(team: MTeam, role: Option<TeamRole>) -> Self {
        Self {
            new_project_role: team
                .new_project_role
                .and_then(builtin_role_name)
                .map(str::to_string),
            id: team.id,
            name: team.name,
            display_name: team.display_name,
            managed: team.managed,
            role,
            oidc_group: team.oidc_group,
            scim_group: team.scim_group,
            new_project_users: team.new_project_users,
            new_project_workers: team.new_project_workers,
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct MakeTeamRequest {
    pub name: TeamName,
    pub display_name: TeamDisplayName,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PatchTeamRequest {
    pub display_name: Option<TeamDisplayName>,
    pub new_project_users: Option<bool>,
    pub new_project_workers: Option<bool>,
    pub new_project_role: Option<String>,
    pub oidc_group: Option<String>,
    pub scim_group: Option<String>,
}

pub fn builtin_role_id(name: &str) -> Option<RoleId> {
    match name {
        "Admin" => Some(BASE_ROLE_ADMIN_ID),
        "Write" => Some(BASE_ROLE_WRITE_ID),
        "View" => Some(BASE_ROLE_VIEW_ID),
        _ => None,
    }
}

pub fn builtin_role_name(id: RoleId) -> Option<&'static str> {
    ["Admin", "Write", "View"]
        .into_iter()
        .find(|name| builtin_role_id(name) == Some(id))
}

fn blank_to_none(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[utoipa::path(
    get,
    path = "/teams",
    operation_id = "listTeams",
    tag = "teams",
    summary = "List my teams",
    description = "Teams the caller is a member of, with the caller's role, sorted by name.",
    responses((status = OK, description = "OK", body = Vec<TeamSummary>), WebError)
)]
pub async fn get_teams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<TeamSummary>>>> {
    let rows = gradient_db::teams::members::teams_of_user(&state.web_db, user.id).await?;

    let mut items: Vec<TeamSummary> = rows
        .into_iter()
        .filter_map(|(membership, team)| team.map(|team| TeamSummary::new(team, membership.role)))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ok_json(items))
}

#[utoipa::path(
    get,
    path = "/teams/available",
    operation_id = "teamNameAvailable",
    tag = "teams",
    summary = "Check a team name",
    params(("name" = String, Query)),
    responses((status = OK, description = "Whether the name is valid and free", body = bool, content_type = "application/json"), WebError)
)]
pub async fn get_team_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    let Ok(name) = TeamName::try_from(name) else {
        return Ok(ok_json(false));
    };
    let taken = gradient_db::teams::by_name(&state.web_db, name.as_str())
        .await?
        .is_some();
    Ok(ok_json(!taken))
}

#[utoipa::path(
    put,
    path = "/teams",
    operation_id = "createTeam",
    tag = "teams",
    summary = "Create team",
    description = "Creates a team and makes the caller its Admin. Who may create teams is set by `--permissions-create-team`.",
    request_body = MakeTeamRequest,
    responses((status = OK, description = "Team created", body = String, content_type = "application/json"), WebError)
)]
pub async fn put_team(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    JsonBody(body): JsonBody<MakeTeamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_create_permission(state.config.permissions.create_team, &user)?;

    if gradient_db::teams::by_name(&state.web_db, body.name.as_str())
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::TeamName));
    }

    let tx = state.web_db.inner().begin().await?;
    let team = gradient_db::teams::create(
        &tx,
        MTeam {
            id: TeamId::now_v7(),
            name: body.name.to_string(),
            display_name: body.display_name.trim().to_string(),
            created_by: Some(user.id),
            created_at: gradient_types::now(),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::TeamName))?;

    gradient_db::teams::members::add_member(
        &tx,
        MTeamUser {
            id: TeamUserId::now_v7(),
            team: team.id,
            user: user.id,
            role: TeamRole::Admin,
            source: TeamMemberSource::Api,
        },
    )
    .await?;
    tx.commit().await?;

    audit
        .write(
            Action::TeamCreate,
            EventOwner::default(),
            serde_json::json!({ "team_id": team.id.to_string(), "name": team.name }),
        )
        .await;

    Ok(ok_json(team.id.to_string()))
}

#[utoipa::path(
    get,
    path = "/teams/{team}",
    operation_id = "getTeam",
    tag = "teams",
    summary = "Get team",
    description = "Members of the team and superusers can read it.",
    params(TeamPath),
    responses((status = OK, description = "OK", body = TeamResponse), WebError)
)]
pub async fn get_team(
    TeamScope { team, role, .. }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<TeamResponse>>> {
    Ok(ok_json(TeamResponse::new(team, role)))
}

#[utoipa::path(
    patch,
    path = "/teams/{team}",
    operation_id = "patchTeam",
    tag = "teams",
    summary = "Change team",
    description = "Needs Admin in the team. Only superusers set `oidc_group`, `scim_group` and the `new_project_*` fields. State-managed teams refuse changes.",
    params(TeamPath),
    request_body = PatchTeamRequest,
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_team(
    TeamScope { team, user, db, .. }: TeamScope<AdminUnmanaged>,
    audit: Audit,
    JsonBody(body): JsonBody<PatchTeamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if body.oidc_group.is_some()
        || body.scim_group.is_some()
        || body.new_project_users.is_some()
        || body.new_project_workers.is_some()
        || body.new_project_role.is_some()
    {
        require_superuser(&user)?;
    }

    let team_id = team.id;
    let mut active: ATeam = team.into();
    if let Some(display_name) = body.display_name {
        active.display_name = Set(display_name.trim().to_string());
    }
    if let Some(users) = body.new_project_users {
        active.new_project_users = Set(users);
    }
    if let Some(workers) = body.new_project_workers {
        active.new_project_workers = Set(workers);
    }
    if let Some(role) = body.new_project_role {
        let role = blank_to_none(role)
            .map(|name| builtin_role_id(&name).ok_or(TeamRefusal::InvalidNewProjectRole))
            .transpose()?;
        active.new_project_role = Set(role);
    }
    if let Some(group) = body.oidc_group {
        active.oidc_group = Set(blank_to_none(group));
    }
    if let Some(group) = body.scim_group {
        active.scim_group = Set(blank_to_none(group));
    }
    gradient_db::teams::update(&db.web_db, active)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::ScimGroup))?;

    audit
        .write(
            Action::TeamUpdate,
            EventOwner::default(),
            serde_json::json!({ "team_id": team_id.to_string() }),
        )
        .await;

    Ok(ok_json("Team updated".to_string()))
}

#[utoipa::path(
    delete,
    path = "/teams/{team}",
    operation_id = "deleteTeam",
    tag = "teams",
    summary = "Delete team",
    description = "Needs Admin in the team. Deletes its grants, requests, invitations and workers; connected team workers lose those projects.",
    params(TeamPath),
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_team(
    TeamScope { team, db, .. }: TeamScope<AdminUnmanaged>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let workers = gradient_db::teams::team_worker_ids(&db.web_db, team.id).await?;
    let projects: HashSet<ProjectId> =
        gradient_db::teams::workers::projects_granted_with_workers(&db.web_db, team.id)
            .await?
            .into_iter()
            .collect();
    let team_id = team.id;
    gradient_db::teams::delete(&db.web_db, team).await?;
    withdraw_team_workers(&scheduler, &workers, &projects).await;

    audit
        .write(
            Action::TeamDelete,
            EventOwner::default(),
            serde_json::json!({ "team_id": team_id.to_string() }),
        )
        .await;

    Ok(ok_json("Team deleted".to_string()))
}
