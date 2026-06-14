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
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use gradient_core::ServerState;
use gradient_entity::organization_user;
use gradient_types::{OrganizationId, RoleId, UserId};

use super::dto::*;
use super::error::{SCIM_CONTENT_TYPE, ScimError, ScimResult};
use super::filter::parse_eq_filter;

fn scim_json(status: StatusCode, body: impl serde::Serialize) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, SCIM_CONTENT_TYPE)],
        Json(serde_json::to_value(body).unwrap()),
    )
        .into_response()
}

fn grants<'a>(
    state: &'a Arc<ServerState>,
    group: &str,
) -> Option<&'a Vec<(OrganizationId, RoleId)>> {
    state.scim_group_roles.get(group)
}

async fn group_resource(state: &Arc<ServerState>, group: &str) -> ScimResult<GroupResource> {
    let grants = grants(state, group).ok_or_else(|| ScimError::not_found("group not found"))?;
    let db = state.web_db.inner();
    let mut members: Vec<GroupMember> = Vec::new();
    // A member holds every grant; the first grant is representative.
    if let Some((org, role)) = grants.first() {
        let rows = organization_user::Entity::find()
            .filter(organization_user::Column::Organization.eq(*org))
            .filter(organization_user::Column::Role.eq(*role))
            .all(db)
            .await?;
        for r in rows {
            members.push(GroupMember {
                value: r.user.to_string(),
                display: None,
            });
        }
    }

    Ok(GroupResource {
        schemas: [GROUP_SCHEMA],
        id: group.to_string(),
        display_name: group.to_string(),
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
    let names: Vec<String> = match q.filter.as_deref().and_then(parse_eq_filter) {
        Some((attr, val)) if attr == "displayname" => vec![val],
        Some(_) => return Err(ScimError::bad_request("invalidFilter", "unsupported filter")),
        None => state.scim_group_roles.keys().cloned().collect(),
    };

    let mut resources = Vec::new();
    for name in names {
        if grants(&state, &name).is_some() {
            resources.push(group_resource(&state, &name).await?);
        }
    }

    let total = resources.len();
    Ok(scim_json(
        StatusCode::OK,
        ListResponse::new(resources, total, 1),
    ))
}

pub async fn get(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ScimResult<impl IntoResponse> {
    Ok(scim_json(StatusCode::OK, group_resource(&state, &id).await?))
}

pub async fn patch(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<PatchRequest>,
) -> ScimResult<impl IntoResponse> {
    let grants = grants(&state, &id)
        .ok_or_else(|| ScimError::not_found("group not found"))?
        .clone();
    for op in &body.operations {
        let path = op.path.as_deref().unwrap_or("").to_ascii_lowercase();
        if !path.is_empty() && !path.starts_with("members") {
            continue;
        }

        let member_ids = extract_member_ids(op);
        match op.op.to_ascii_lowercase().as_str() {
            "add" | "replace" => {
                for uid in &member_ids {
                    add_member(&state, &grants, uid).await?;
                }
            }
            "remove" => {
                for uid in member_ids_for_remove(op, &member_ids) {
                    remove_member(&state, &grants, &uid).await?;
                }
            }
            _ => {}
        }
    }

    Ok(scim_json(StatusCode::OK, group_resource(&state, &id).await?))
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
    // Okta/Entra remove uses path `members[value eq "<id>"]` or a value array.
    if !parsed.is_empty() {
        return parsed.to_vec();
    }

    op.path
        .as_deref()
        .and_then(|p| p.split('"').nth(1).map(String::from))
        .into_iter()
        .collect()
}

async fn add_member(
    state: &Arc<ServerState>,
    grants: &[(OrganizationId, RoleId)],
    uid: &str,
) -> ScimResult<()> {
    let user_id = parse_uid(uid)?;
    let db = state.web_db.inner();
    for (org, role) in grants {
        let exists = organization_user::Entity::find()
            .filter(organization_user::Column::Organization.eq(*org))
            .filter(organization_user::Column::User.eq(user_id))
            .one(db)
            .await?;
        match exists {
            Some(m) => {
                let mut am: organization_user::ActiveModel = m.into();
                am.role = Set(*role);
                am.update(db).await?;
            }
            None => {
                organization_user::ActiveModel {
                    organization: Set(*org),
                    user: Set(user_id),
                    role: Set(*role),
                    ..Default::default()
                }
                .insert(db)
                .await?;
            }
        }
    }

    Ok(())
}

async fn remove_member(
    state: &Arc<ServerState>,
    grants: &[(OrganizationId, RoleId)],
    uid: &str,
) -> ScimResult<()> {
    let user_id = parse_uid(uid)?;
    let db = state.web_db.inner();
    for (org, _role) in grants {
        organization_user::Entity::delete_many()
            .filter(organization_user::Column::Organization.eq(*org))
            .filter(organization_user::Column::User.eq(user_id))
            .exec(db)
            .await?;
    }

    Ok(())
}

fn parse_uid(uid: &str) -> ScimResult<UserId> {
    uid.parse::<UserId>()
        .map_err(|_| ScimError::bad_request("invalidValue", "member value must be a user id"))
}
