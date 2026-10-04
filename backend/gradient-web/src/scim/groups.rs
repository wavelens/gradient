/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};

use gradient_core::ServerState;
use gradient_types::*;

use super::dto::*;
use super::error::{SCIM_CONTENT_TYPE, ScimError, ScimResult};
use super::filter::parse_eq_filter;

fn scim_json(status: StatusCode, body: impl serde::Serialize) -> Response {
    match serde_json::to_value(body) {
        Ok(value) => (
            status,
            [(header::CONTENT_TYPE, SCIM_CONTENT_TYPE)],
            Json(value),
        )
            .into_response(),
        Err(e) => {
            ScimError::internal(format!("failed to serialise SCIM response: {e}")).into_response()
        }
    }
}

async fn team_for_group(state: &Arc<ServerState>, group: &str) -> ScimResult<MTeam> {
    ETeam::find()
        .filter(CTeam::ScimGroup.eq(group))
        .one(state.web_db.inner())
        .await?
        .ok_or_else(|| ScimError::not_found("group not found"))
}

async fn group_resource(state: &Arc<ServerState>, team: &MTeam) -> ScimResult<GroupResource> {
    let group = team.scim_group.clone().unwrap_or_default();
    let members = ETeamUser::find()
        .filter(CTeamUser::Team.eq(team.id))
        .all(state.web_db.inner())
        .await?
        .into_iter()
        .map(|membership| GroupMember {
            value: membership.user.to_string(),
            display: None,
        })
        .collect();

    Ok(GroupResource {
        schemas: [GROUP_SCHEMA],
        id: group.clone(),
        display_name: group,
        members,
        meta: Meta {
            resource_type: "Group",
        },
    })
}

#[derive(serde::Deserialize)]
pub struct ListQuery {
    pub filter: Option<String>,
}

pub async fn list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<ListQuery>,
) -> ScimResult<impl IntoResponse> {
    let filter = match q.filter.as_deref().and_then(parse_eq_filter) {
        Some((attr, val)) if attr == "displayname" => CTeam::ScimGroup.eq(val),
        Some(_) => {
            return Err(ScimError::bad_request(
                "invalidFilter",
                "unsupported filter",
            ));
        }
        None => CTeam::ScimGroup.is_not_null(),
    };
    let teams = ETeam::find()
        .filter(filter)
        .all(state.web_db.inner())
        .await?;

    let mut resources = Vec::with_capacity(teams.len());
    for team in &teams {
        resources.push(group_resource(&state, team).await?);
    }
    let total = resources.len();
    Ok(scim_json(
        StatusCode::OK,
        super::dto::ListResponse::new(resources, total, 1),
    ))
}

pub async fn get(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ScimResult<impl IntoResponse> {
    let team = team_for_group(&state, &id).await?;
    Ok(scim_json(
        StatusCode::OK,
        group_resource(&state, &team).await?,
    ))
}

pub async fn patch(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<PatchRequest>,
) -> ScimResult<impl IntoResponse> {
    let team = team_for_group(&state, &id).await?;
    for op in &body.operations {
        let path = op.path.as_deref().unwrap_or("").to_ascii_lowercase();
        if !path.is_empty() && !path.starts_with("members") {
            continue;
        }

        let member_ids = extract_member_ids(op);
        match op.op.to_ascii_lowercase().as_str() {
            "add" | "replace" => {
                for uid in &member_ids {
                    add_member(&state, team.id, uid).await?;
                }
            }
            "remove" => {
                for uid in member_ids_for_remove(op, &member_ids) {
                    remove_member(&state, team.id, &uid).await?;
                }
            }
            _ => {}
        }
    }

    Ok(scim_json(
        StatusCode::OK,
        group_resource(&state, &team).await?,
    ))
}

fn extract_member_ids(op: &PatchOperation) -> Vec<String> {
    let Some(value) = &op.value else {
        return Vec::new();
    };
    if let Some(arr) = value.as_array() {
        arr.iter()
            .filter_map(|m| m.get("value").and_then(|v| v.as_str()).map(String::from))
            .collect()
    } else if let Some(s) = value.as_str() {
        vec![s.to_string()]
    } else {
        Vec::new()
    }
}

fn member_ids_for_remove(op: &PatchOperation, parsed: &[String]) -> Vec<String> {
    // Okta and Entra are removing members via path `members[value eq "<id>"]` or a value array.
    if !parsed.is_empty() {
        return parsed.to_vec();
    }

    op.path
        .as_deref()
        .and_then(|p| p.split('"').nth(1).map(String::from))
        .into_iter()
        .collect()
}

async fn add_member(state: &Arc<ServerState>, team: TeamId, uid: &str) -> ScimResult<()> {
    let user = parse_uid(uid)?;
    let db = state.web_db.inner();
    let existing = ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::User.eq(user))
        .one(db)
        .await?;
    if existing.is_none() {
        MTeamUser {
            id: TeamUserId::now_v7(),
            team,
            user,
            role: TeamRole::Member,
            via_group: true,
        }
        .into_active_model()
        .insert(db)
        .await?;
    }
    Ok(())
}

async fn remove_member(state: &Arc<ServerState>, team: TeamId, uid: &str) -> ScimResult<()> {
    let user = parse_uid(uid)?;
    ETeamUser::delete_many()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::User.eq(user))
        .filter(CTeamUser::ViaGroup.eq(true))
        .exec(state.web_db.inner())
        .await?;
    Ok(())
}

fn parse_uid(uid: &str) -> ScimResult<UserId> {
    uid.parse::<UserId>()
        .map_err(|_| ScimError::bad_request("invalidValue", "member value must be a user id"))
}
