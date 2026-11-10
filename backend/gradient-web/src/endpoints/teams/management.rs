/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TeamRefusal;
use super::workers::withdraw_team_workers;
use crate::api_spec::TeamPath;
use crate::audit::Audit;
use crate::authorization::{MaybeApiKey, reject_api_key_authentication};
use crate::error::{WebError, WebResult, require_create_permission, require_superuser};
use crate::helpers::{CreatedEntityResponse, ok_json};
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

pub(crate) async fn lookup_owner_team_id_and_check_user_is_member_or_admin(
    db: &gradient_db::WebDb,
    user: &MUser,
    name: &str,
) -> WebResult<TeamId> {
    let team = gradient_db::teams::by_name(db, name)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::Team))?;
    if !user.superuser
        && gradient_db::teams::team_role_of(db, team.id, user.id)
            .await?
            .is_none()
    {
        return Err(crate::refusal::SharedRefusal::NotMemberOfOwnerTeam.into());
    }
    Ok(team.id)
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
    description = "Teams that include the caller as a member, with the caller's role, sorted by display name. Callers need a signed-in session. Team endpoints don't accept API keys.",
    responses((status = OK, description = "OK", body = Vec<TeamSummary>), WebError)
)]
pub async fn get_teams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(MaybeApiKey(api_key)): Extension<MaybeApiKey>,
) -> WebResult<Json<BaseResponse<Vec<TeamSummary>>>> {
    reject_api_key_authentication(api_key.as_ref())?;
    let rows = gradient_db::teams::members::teams_of_user(&state.web_db, user.id).await?;

    let mut items: Vec<TeamSummary> = rows
        .into_iter()
        .filter_map(|(membership, team)| team.map(|team| TeamSummary::new(team, membership.role)))
        .collect();
    items.sort_by(|a, b| (&a.display_name, &a.name).cmp(&(&b.display_name, &b.name)));

    Ok(ok_json(items))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TeamGrantPickerRow {
    pub name: String,
    pub display_name: String,
    pub member_count: u64,
    pub worker_count: u64,
    pub caller_is_admin: bool,
}

#[utoipa::path(
    get,
    path = "/teams/all",
    operation_id = "listAllTeams",
    tag = "teams",
    summary = "List all teams",
    description = "List all teams in the Grant Team picker for a project or cache. Response fields include the number of members, the number of workers, and whether the caller is an admin of that team. Grants to a team where the caller isn't an admin require approval from that team.",
    responses((status = OK, description = "OK", body = Vec<TeamGrantPickerRow>), WebError)
)]
pub async fn get_all_teams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(MaybeApiKey(api_key)): Extension<MaybeApiKey>,
) -> WebResult<Json<BaseResponse<Vec<TeamGrantPickerRow>>>> {
    reject_api_key_authentication(api_key.as_ref())?;
    let rows =
        gradient_db::teams::all_teams_with_counts_and_role_of_user(&state.web_db, user.id).await?;

    Ok(ok_json(
        rows.into_iter()
            .map(|row| TeamGrantPickerRow {
                caller_is_admin: user.superuser || row.user_role == Some(TeamRole::Admin),
                name: row.team.name,
                display_name: row.team.display_name,
                member_count: row.member_count,
                worker_count: row.worker_count,
            })
            .collect(),
    ))
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
    description = "Create a team and make the caller its admin. Use `--permissions-create-team` to control which users can create teams. Requests need a signed-in session. API keys cannot use team endpoints.",
    request_body = MakeTeamRequest,
    responses((status = OK, description = "Team created", body = CreatedEntityResponse), WebError)
)]
pub async fn put_team(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(MaybeApiKey(api_key)): Extension<MaybeApiKey>,
    JsonBody(body): JsonBody<MakeTeamRequest>,
) -> WebResult<Json<BaseResponse<CreatedEntityResponse>>> {
    reject_api_key_authentication(api_key.as_ref())?;
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

    let url = frontend_page_urls::team(&state.config.server.frontend_url, &team.name);
    Ok(ok_json(CreatedEntityResponse::new(team.id, team.name, url)))
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
    description = "Callers need Admin access in the team. Only superusers set the `oidc_group`, `scim_group`, and `new_project_*` fields. Set `new_project_users` only together with `new_project_role`. Teams under state management reject changes.",
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
    if *active.new_project_users.as_ref() && active.new_project_role.as_ref().is_none() {
        return Err(TeamRefusal::RoleRequiredForUsers.into());
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
