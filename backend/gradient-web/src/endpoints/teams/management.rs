/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TeamRefusal;
use super::workers::withdraw_team_workers;
use crate::audit::{RequestInfo, record as audit_record};
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
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QuerySelect, RelationTrait, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Serialize)]
pub struct TeamSummary {
    pub name: String,
    pub display_name: String,
    pub role: TeamRole,
}

#[derive(Serialize)]
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

#[derive(Deserialize)]
pub struct MakeTeamRequest {
    pub name: TeamName,
    pub display_name: TeamDisplayName,
}

#[derive(Deserialize)]
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

fn team_response(team: MTeam, role: Option<TeamRole>) -> TeamResponse {
    TeamResponse {
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

pub async fn get_teams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<TeamSummary>>>> {
    let rows = ETeamUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_user::Relation::Team.def(),
        )
        .select_also(gradient_entity::team::Entity)
        .filter(CTeamUser::User.eq(user.id))
        .all(&state.web_db)
        .await?;

    let mut items: Vec<TeamSummary> = rows
        .into_iter()
        .filter_map(|(membership, team)| {
            team.map(|team| TeamSummary {
                name: team.name,
                display_name: team.display_name,
                role: membership.role,
            })
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ok_json(items))
}

pub async fn get_team_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    let Ok(name) = TeamName::try_from(name) else {
        return Ok(ok_json(false));
    };
    let taken = ETeam::find()
        .filter(CTeam::Name.eq(name.as_str()))
        .one(&state.web_db)
        .await?
        .is_some();
    Ok(ok_json(!taken))
}

pub async fn put_team(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    JsonBody(body): JsonBody<MakeTeamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_create_permission(state.config.permissions.create_team, &user)?;

    if ETeam::find()
        .filter(CTeam::Name.eq(body.name.as_str()))
        .one(&state.web_db)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::TeamName));
    }

    let tx = state.web_db.inner().begin().await?;
    let team = MTeam {
        id: TeamId::now_v7(),
        name: body.name.to_string(),
        display_name: body.display_name.trim().to_string(),
        created_by: Some(user.id),
        created_at: gradient_types::now(),
        ..Default::default()
    }
    .into_active_model()
    .insert(&tx)
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::TeamName))?;

    MTeamUser {
        id: TeamUserId::now_v7(),
        team: team.id,
        user: user.id,
        role: TeamRole::Admin,
        source: TeamMemberSource::Api,
    }
    .into_active_model()
    .insert(&tx)
    .await?;
    tx.commit().await?;

    audit_record(
        &state.db(),
        Some(user.id),
        Action::TeamCreate,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "name": team.name })),
    )
    .await;

    Ok(ok_json(team.id.to_string()))
}

pub async fn get_team(
    TeamScope { team, role, .. }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<TeamResponse>>> {
    Ok(ok_json(team_response(team, role)))
}

pub async fn get_team_evaluations(
    TeamScope { team, db, .. }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<gradient_db::teams::evaluations::TeamEvaluation>>>> {
    Ok(ok_json(
        gradient_db::teams::evaluations::recent_evaluations(&db.web_db, team.id, 10).await?,
    ))
}

pub async fn patch_team(
    TeamScope { team, user, db, .. }: TeamScope<AdminUnmanaged>,
    info: RequestInfo,
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
    active
        .update(&db.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::ScimGroup))?;

    audit_record(
        &db,
        Some(user.id),
        Action::TeamUpdate,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({ "team_id": team_id.to_string() })),
    )
    .await;

    Ok(ok_json("Team updated".to_string()))
}

pub async fn delete_team(
    TeamScope { team, user, db, .. }: TeamScope<AdminUnmanaged>,
    info: RequestInfo,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let workers = gradient_db::teams::team_worker_ids(&db.web_db, team.id).await?;
    let projects: HashSet<ProjectId> =
        gradient_db::teams::workers::projects_granted_with_workers(&db.web_db, team.id)
            .await?
            .into_iter()
            .collect();
    let team_id = team.id;
    team.into_active_model().delete(&db.web_db).await?;
    withdraw_team_workers(&scheduler, &workers, &projects).await;

    audit_record(
        &db,
        Some(user.id),
        Action::TeamDelete,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({ "team_id": team_id.to_string() })),
    )
    .await;

    Ok(ok_json("Team deleted".to_string()))
}
