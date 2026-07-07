/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use serde::Deserialize;

use gradient_core::ServerState;
use gradient_entity::user::{
    ActiveModel as UserActive, Column as UserColumn, Entity as UserEntity, Model as UserModel,
};
use gradient_types::{UserId, now};

use super::dto::*;
use super::error::{SCIM_CONTENT_TYPE, ScimError, ScimResult};
use super::filter::parse_eq_filter;

fn user_resource(u: &UserModel) -> UserResource {
    UserResource {
        schemas: [USER_SCHEMA],
        id: u.id.to_string(),
        user_name: u.username.clone(),
        external_id: u.scim_external_id.clone(),
        name: NameResource {
            formatted: u.name.clone(),
        },
        display_name: u.name.clone(),
        emails: vec![EmailResource {
            value: u.email.clone(),
            primary: true,
        }],
        active: u.active,
        meta: Meta {
            resource_type: "User",
        },
    }
}

fn scim_json(status: StatusCode, body: impl serde::Serialize) -> Response {
    (
        status,
        [(axum::http::header::CONTENT_TYPE, SCIM_CONTENT_TYPE)],
        Json(serde_json::to_value(body).unwrap()),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub filter: Option<String>,
    #[serde(rename = "startIndex")]
    pub start_index: Option<i64>,
    pub count: Option<i64>,
}

pub async fn list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<ListQuery>,
) -> ScimResult<impl IntoResponse> {
    let db = state.web_db.inner();
    let mut query = UserEntity::find().filter(UserColumn::Managed.eq(true));

    if let Some(f) = q.filter.as_deref() {
        match parse_eq_filter(f) {
            Some((attr, val)) if attr == "username" => {
                query = query.filter(UserColumn::Username.eq(val));
            }
            Some((attr, val)) if attr == "externalid" => {
                query = query.filter(UserColumn::ScimExternalId.eq(val));
            }
            _ => {
                return Err(ScimError::bad_request(
                    "invalidFilter",
                    "unsupported filter",
                ));
            }
        }
    }

    let start = q.start_index.unwrap_or(1).max(1) as u64;
    let count = q.count.unwrap_or(100).clamp(0, 200) as u64;
    let total = query.clone().count(db).await? as usize;
    let rows = if count == 0 {
        Vec::new()
    } else {
        query
            .paginate(db, count)
            .fetch_page((start - 1) / count.max(1))
            .await?
    };

    let resources: Vec<_> = rows.iter().map(user_resource).collect();
    Ok(scim_json(
        StatusCode::OK,
        ListResponse::new(resources, total, start as usize),
    ))
}

pub async fn get(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state, &id).await?;
    Ok(scim_json(StatusCode::OK, user_resource(&u)))
}

pub async fn create(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<UserRequest>,
) -> ScimResult<impl IntoResponse> {
    let db = state.web_db.inner();
    if UserEntity::find()
        .filter(UserColumn::Username.eq(body.user_name.clone()))
        .one(db)
        .await?
        .is_some()
    {
        return Err(ScimError::conflict("userName already exists"));
    }

    let email = body
        .emails
        .iter()
        .find(|e| e.primary)
        .or_else(|| body.emails.first())
        .map(|e| e.value.clone())
        .unwrap_or_default();
    let display = body
        .display_name
        .clone()
        .or_else(|| body.name.as_ref().and_then(|n| n.formatted.clone()))
        .unwrap_or_else(|| body.user_name.clone());

    let model = UserActive {
        username: Set(body.user_name.clone()),
        name: Set(display),
        email: Set(email),
        password: Set(None),
        last_login_at: Set(now()),
        created_at: Set(now()),
        email_verified: Set(true),
        managed: Set(true),
        active: Set(body.active),
        scim_external_id: Set(body.external_id.clone()),
        ..Default::default()
    };
    let u = model.insert(db).await?;
    Ok(scim_json(StatusCode::CREATED, user_resource(&u)))
}

pub async fn replace(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<UserRequest>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state, &id).await?;
    let mut active: UserActive = u.into();
    active.name = Set(body
        .display_name
        .clone()
        .or_else(|| body.name.as_ref().and_then(|n| n.formatted.clone()))
        .unwrap_or_else(|| body.user_name.clone()));
    if let Some(e) = body
        .emails
        .iter()
        .find(|e| e.primary)
        .or_else(|| body.emails.first())
    {
        active.email = Set(e.value.clone());
    }

    active.active = Set(body.active);
    let u = active.update(state.web_db.inner()).await?;
    Ok(scim_json(StatusCode::OK, user_resource(&u)))
}

pub async fn patch(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<PatchRequest>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state, &id).await?;
    let mut active: UserActive = u.into();
    for op in &body.operations {
        if !matches!(op.op.to_ascii_lowercase().as_str(), "replace" | "add") {
            continue;
        }

        apply_user_patch(&mut active, op)?;
    }

    let u = active.update(state.web_db.inner()).await?;
    Ok(scim_json(StatusCode::OK, user_resource(&u)))
}

pub async fn delete(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state, &id).await?;
    let hard = state
        .config
        .scim
        .as_ref()
        .map(|s| s.hard_delete)
        .unwrap_or(false);
    if hard {
        UserEntity::delete_by_id(u.id)
            .exec(state.web_db.inner())
            .await?;
    } else {
        let mut active: UserActive = u.into();
        active.active = Set(false);
        active.update(state.web_db.inner()).await?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn find_user(state: &Arc<ServerState>, id: &str) -> ScimResult<UserModel> {
    let user_id = id
        .parse::<UserId>()
        .map_err(|_| ScimError::not_found("user not found"))?;
    UserEntity::find_by_id(user_id)
        .filter(UserColumn::Managed.eq(true))
        .one(state.web_db.inner())
        .await?
        .ok_or_else(|| ScimError::not_found("user not found"))
}

fn apply_user_patch(active: &mut UserActive, op: &PatchOperation) -> ScimResult<()> {
    let path = op.path.as_deref().unwrap_or("").to_ascii_lowercase();
    let value = op.value.clone().unwrap_or(serde_json::Value::Null);
    match path.as_str() {
        "active" => {
            let v = value
                .as_bool()
                .ok_or_else(|| ScimError::bad_request("invalidValue", "active must be boolean"))?;
            active.active = Set(v);
        }
        "displayname" | "name.formatted" => {
            if let Some(s) = value.as_str() {
                active.name = Set(s.to_string());
            }
        }
        // No-path replace: value is an object of attributes.
        "" => {
            if let Some(obj) = value.as_object() {
                if let Some(a) = obj.get("active").and_then(|v| v.as_bool()) {
                    active.active = Set(a);
                }
                if let Some(d) = obj.get("displayName").and_then(|v| v.as_str()) {
                    active.name = Set(d.to_string());
                }
            }
        }
        _ => {}
    }

    Ok(())
}
