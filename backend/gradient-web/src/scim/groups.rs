/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::{ConnectionTrait, TransactionTrait};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

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

async fn team_for_group<C: ConnectionTrait>(db: &C, group: &str) -> ScimResult<MTeam> {
    gradient_db::teams::by_scim_group(db, group)
        .await?
        .ok_or_else(|| ScimError::not_found("group not found"))
}

async fn group_resource<C: ConnectionTrait>(db: &C, team: &MTeam) -> ScimResult<GroupResource> {
    let memberships = gradient_db::teams::members::scim_memberships_of_team(db, team.id).await?;

    Ok(GroupResource::new(team, memberships))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GroupListQuery {
    pub filter: Option<String>,
}

#[utoipa::path(
    get,
    path = "/Groups",
    operation_id = "scimListGroups",
    tag = "scim",
    summary = "List SCIM groups",
    description = "List SCIM groups. A SCIM group is a team with `scim_group` set. SCIM can't create groups. Group members are team members whose accounts SCIM provisioned. The response leaves out members from another method. The endpoint can take `displayName eq \"...\"` filters.",
    params(GroupListQuery),
    responses((status = OK, description = "Group list", body = super::dto::ListResponse<GroupResource>, content_type = "application/scim+json"), ScimError)
)]
pub async fn list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<GroupListQuery>,
) -> ScimResult<impl IntoResponse> {
    let group = match q.filter.as_deref().and_then(parse_eq_filter) {
        Some((attr, val)) if attr == "displayname" => Some(val),
        Some(_) => {
            return Err(ScimError::bad_request(
                "invalidFilter",
                "unsupported filter",
            ));
        }
        None => None,
    };
    let teams =
        gradient_db::teams::linked_to_scim_group(state.web_db.inner(), group.as_deref()).await?;

    let mut resources = Vec::with_capacity(teams.len());
    for team in &teams {
        resources.push(group_resource(&state.web_db, team).await?);
    }
    let total = resources.len();
    Ok(scim_json(
        StatusCode::OK,
        super::dto::ListResponse::new(resources, total, 1),
    ))
}

#[utoipa::path(
    get,
    path = "/Groups/{id}",
    operation_id = "scimGetGroup",
    tag = "scim",
    summary = "Get a SCIM group",
    description = "Unknown group names return 404.",
    params(("id" = String, Path)),
    responses((status = OK, description = "Group", body = GroupResource, content_type = "application/scim+json"), ScimError)
)]
pub async fn get(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ScimResult<impl IntoResponse> {
    let team = team_for_group(&state.web_db, &id).await?;
    Ok(scim_json(
        StatusCode::OK,
        group_resource(&state.web_db, &team).await?,
    ))
}

#[utoipa::path(
    patch,
    path = "/Groups/{id}",
    operation_id = "scimPatchGroup",
    tag = "scim",
    summary = "Patch SCIM group membership",
    description = "Use this endpoint to add, remove, or replace members of the team named in `scim_group`. Users from the add operation join as members. Use `replace` with `members` to remove members that SCIM added and that the new list doesn't contain. Members added manually or by the state remain. Set member values in `add` and `replace` to ids of users that SCIM provisioned. Other ids return 400. Let `remove` skip ids that don't belong to such users. Rejected operations leave the whole request unapplied. Return 404 for an unknown group name.",
    params(("id" = String, Path)),
    request_body(content = PatchRequest, content_type = "application/scim+json"),
    responses((status = OK, description = "Group patched", body = GroupResource, content_type = "application/scim+json"), ScimError)
)]
pub async fn patch(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<PatchRequest>,
) -> ScimResult<impl IntoResponse> {
    let tx = state.web_db.inner().begin().await?;
    let team = team_for_group(&tx, &id).await?;
    let mut resolved_operations = Vec::with_capacity(body.operations.len());
    for op in &body.operations {
        let path = op.path.as_deref().unwrap_or("").to_ascii_lowercase();
        if !path.is_empty() && !path.starts_with("members") {
            return Err(ScimError::unsupported_path());
        }

        let member_ids = extract_member_ids(op);
        resolved_operations.push(match op.op.to_ascii_lowercase().as_str() {
            "add" => ScimGroupPatchOperationWithResolvedUserIds::Add(
                parse_scim_group_member_ids_to_provisioned_user_ids(&tx, &member_ids).await?,
            ),
            "replace" => {
                let members =
                    replacement_member_ids(op, &path).ok_or_else(ScimError::unsupported_path)?;
                ScimGroupPatchOperationWithResolvedUserIds::Replace(
                    parse_scim_group_member_ids_to_provisioned_user_ids(&tx, &members).await?,
                )
            }
            "remove" => ScimGroupPatchOperationWithResolvedUserIds::Remove(
                filter_scim_group_remove_member_ids_to_scim_provisioned_user_ids(
                    &tx,
                    &member_ids_for_remove(op, &member_ids),
                )
                .await?,
            ),
            _ => return Err(ScimError::unsupported_operation()),
        });
    }

    for operation in resolved_operations {
        match operation {
            ScimGroupPatchOperationWithResolvedUserIds::Add(users) => {
                for user in users {
                    add_member(&tx, team.id, user).await?;
                }
            }
            ScimGroupPatchOperationWithResolvedUserIds::Replace(wanted) => {
                replace_members(&tx, team.id, &wanted).await?;
            }
            ScimGroupPatchOperationWithResolvedUserIds::Remove(users) => {
                for user in users {
                    gradient_db::teams::members::remove_group_member(&tx, team.id, user).await?;
                }
            }
        }
    }

    let group = group_resource(&tx, &team).await?;
    tx.commit().await?;
    Ok(scim_json(StatusCode::OK, group))
}

enum ScimGroupPatchOperationWithResolvedUserIds {
    Add(Vec<UserId>),
    Replace(Vec<UserId>),
    Remove(Vec<UserId>),
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

async fn add_member<C: ConnectionTrait>(db: &C, team: TeamId, user: UserId) -> ScimResult<()> {
    gradient_db::teams::members::add_team_user_if_absent(
        db,
        MTeamUser {
            id: TeamUserId::now_v7(),
            team,
            user,
            role: TeamRole::Member,
            source: TeamMemberSource::ScimGroup,
        },
    )
    .await?;
    Ok(())
}

fn replacement_member_ids(op: &PatchOperation, path: &str) -> Option<Vec<String>> {
    if !path.is_empty() {
        return Some(extract_member_ids(op));
    }
    let members = op.value.as_ref()?.get("members")?.as_array()?;
    Some(
        members
            .iter()
            .filter_map(|m| m.get("value").and_then(|v| v.as_str()).map(String::from))
            .collect(),
    )
}

async fn replace_members<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    wanted: &[UserId],
) -> ScimResult<()> {
    for user in wanted {
        add_member(db, team, *user).await?;
    }
    for membership in gradient_db::teams::members::scim_memberships_of_team(db, team).await? {
        if membership.source == TeamMemberSource::ScimGroup && !wanted.contains(&membership.user) {
            gradient_db::teams::members::remove_group_member(db, team, membership.user).await?;
        }
    }
    Ok(())
}

async fn parse_scim_group_member_ids_to_provisioned_user_ids<C: ConnectionTrait>(
    db: &C,
    uids: &[String],
) -> ScimResult<Vec<UserId>> {
    let mut users = Vec::with_capacity(uids.len());
    for uid in uids {
        users.push(parse_scim_group_member_id_to_provisioned_user_id(db, uid).await?);
    }
    Ok(users)
}

async fn filter_scim_group_remove_member_ids_to_scim_provisioned_user_ids<C: ConnectionTrait>(
    db: &C,
    uids: &[String],
) -> ScimResult<Vec<UserId>> {
    let mut users = Vec::with_capacity(uids.len());
    for user in uids.iter().filter_map(|uid| uid.parse::<UserId>().ok()) {
        if let Some(provisioned) =
            gradient_db::users::fetch_scim_provisioned_user_by_id(db, user).await?
        {
            users.push(provisioned.id);
        }
    }
    Ok(users)
}

async fn parse_scim_group_member_id_to_provisioned_user_id<C: ConnectionTrait>(
    db: &C,
    uid: &str,
) -> ScimResult<UserId> {
    let user = uid
        .parse::<UserId>()
        .map_err(|_| ScimError::bad_request("invalidValue", "member value must be a user id"))?;
    gradient_db::users::fetch_scim_provisioned_user_by_id(db, user)
        .await?
        .map(|provisioned| provisioned.id)
        .ok_or_else(|| {
            ScimError::bad_request(
                "invalidValue",
                "use the ID of a user that SCIM provisioned as the member value",
            )
        })
}
