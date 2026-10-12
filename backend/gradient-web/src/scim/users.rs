/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::WebDb;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sea_orm::Set;
use serde::Deserialize;

use gradient_core::ServerState;
use gradient_db::users::ManagedUserFilter;
use gradient_entity::user::{ActiveModel as UserActive, Model as UserModel};
use gradient_types::{UserId, now};

use super::dto::*;
use super::error::{SCIM_CONTENT_TYPE, ScimError, ScimResult};
use super::filter::parse_eq_filter;
use crate::error::is_unique_violation;

fn scim_json(status: StatusCode, body: impl serde::Serialize) -> Response {
    match serde_json::to_value(body) {
        Ok(value) => (
            status,
            [(axum::http::header::CONTENT_TYPE, SCIM_CONTENT_TYPE)],
            Json(value),
        )
            .into_response(),
        Err(e) => {
            ScimError::internal(format!("failed to serialise SCIM response: {e}")).into_response()
        }
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UserListQuery {
    pub filter: Option<String>,
    #[serde(rename = "startIndex")]
    pub start_index: Option<i64>,
    pub count: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/Users",
    operation_id = "scimListUsers",
    tag = "scim",
    summary = "List / filter SCIM users",
    description = "Lists managed (SCIM-provisioned) users. Supports the `userName eq \"...\"`\nand `externalId eq \"...\"` filter subset plus `startIndex`/`count` pagination.",
    params(UserListQuery),
    responses((status = OK, description = "User list", body = super::dto::ListResponse<UserResource>, content_type = "application/scim+json"), ScimError)
)]
pub async fn list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<UserListQuery>,
) -> ScimResult<impl IntoResponse> {
    let db = state.web_db.inner();
    let filter = match q.filter.as_deref() {
        None => ManagedUserFilter::Any,
        Some(f) => match parse_eq_filter(f) {
            Some((attr, val)) if attr == "username" => ManagedUserFilter::Username(val),
            Some((attr, val)) if attr == "externalid" => ManagedUserFilter::ScimExternalId(val),
            _ => {
                return Err(ScimError::bad_request(
                    "invalidFilter",
                    "unsupported filter",
                ));
            }
        },
    };

    let start = q.start_index.unwrap_or(1).max(1) as u64;
    let count = q.count.unwrap_or(100).clamp(0, 200) as u64;
    let total = usize::try_from(gradient_db::users::count_managed(db, &filter).await?)
        .unwrap_or(usize::MAX);
    let rows = if count == 0 {
        Vec::new()
    } else {
        gradient_db::users::page_of_managed(db, &filter, count, (start - 1) / count.max(1)).await?
    };

    let resources: Vec<_> = rows.iter().map(UserResource::from).collect();
    Ok(scim_json(
        StatusCode::OK,
        ListResponse::new(
            resources,
            total,
            usize::try_from(start).unwrap_or(usize::MAX),
        ),
    ))
}

#[utoipa::path(
    get,
    path = "/Users/{id}",
    operation_id = "scimGetUser",
    tag = "scim",
    summary = "Get a SCIM user",
    params(("id" = String, Path)),
    responses((status = OK, description = "User", body = UserResource, content_type = "application/scim+json"), ScimError)
)]
pub async fn get(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state.web_db, &id).await?;
    Ok(scim_json(StatusCode::OK, UserResource::from(&u)))
}

#[utoipa::path(
    post,
    path = "/Users",
    operation_id = "scimCreateUser",
    tag = "scim",
    summary = "Provision a SCIM user",
    description = "Creates a passwordless `managed` user. Login happens later via OIDC (claimable path). `userName` conflicts return 409.",
    request_body(content = UserRequest, content_type = "application/scim+json"),
    responses((status = CREATED, description = "User created", body = UserResource, content_type = "application/scim+json"), ScimError)
)]
pub async fn create(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<UserRequest>,
) -> ScimResult<impl IntoResponse> {
    let db = state.web_db.inner();
    if gradient_db::users::by_username(db, &body.user_name)
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
    let u = gradient_db::users::insert(db, model).await.map_err(|e| {
        if is_unique_violation(&e) {
            ScimError::conflict("userName already exists")
        } else {
            e.into()
        }
    })?;
    Ok(scim_json(StatusCode::CREATED, UserResource::from(&u)))
}

#[utoipa::path(
    put,
    path = "/Users/{id}",
    operation_id = "scimReplaceUser",
    tag = "scim",
    summary = "Replace a SCIM user",
    params(("id" = String, Path)),
    request_body(content = UserRequest, content_type = "application/scim+json"),
    responses((status = OK, description = "User replaced", body = UserResource, content_type = "application/scim+json"), ScimError)
)]
pub async fn replace(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<UserRequest>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state.web_db, &id).await?;
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
    let u = gradient_db::users::update(state.web_db.inner(), active).await?;
    Ok(scim_json(StatusCode::OK, UserResource::from(&u)))
}

#[utoipa::path(
    patch,
    path = "/Users/{id}",
    operation_id = "scimPatchUser",
    tag = "scim",
    summary = "Patch a SCIM user",
    description = "Applies a SCIM PatchOp. Typically toggles `active` for deactivation/reactivation.",
    params(("id" = String, Path)),
    request_body(content = PatchRequest, content_type = "application/scim+json"),
    responses((status = OK, description = "User patched", body = UserResource, content_type = "application/scim+json"), ScimError)
)]
pub async fn patch(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<PatchRequest>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state.web_db, &id).await?;
    let mut active: UserActive = u.into();
    for op in &body.operations {
        if !matches!(op.op.to_ascii_lowercase().as_str(), "replace" | "add") {
            continue;
        }

        apply_user_patch(&mut active, op)?;
    }

    let u = gradient_db::users::update(state.web_db.inner(), active).await?;
    Ok(scim_json(StatusCode::OK, UserResource::from(&u)))
}

#[utoipa::path(
    delete,
    path = "/Users/{id}",
    operation_id = "scimDeleteUser",
    tag = "scim",
    summary = "Deprovision a SCIM user",
    description = "Soft-disables the user (`active=false`) by default; hard-deletes (cascade)\nwhen `GRADIENT_SCIM_HARD_DELETE=true`.",
    params(("id" = String, Path)),
    responses((status = NO_CONTENT, description = "User deprovisioned"), ScimError)
)]
pub async fn delete(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ScimResult<impl IntoResponse> {
    let u = find_user(&state.web_db, &id).await?;
    let hard = state
        .config
        .scim
        .as_ref()
        .map(|s| s.hard_delete)
        .unwrap_or(false);
    if hard {
        gradient_db::users::delete_by_id(state.web_db.inner(), u.id).await?;
    } else {
        let mut active: UserActive = u.into();
        active.active = Set(false);
        gradient_db::users::update(state.web_db.inner(), active).await?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn find_user(db: &WebDb, id: &str) -> ScimResult<UserModel> {
    let user_id = id
        .parse::<UserId>()
        .map_err(|_| ScimError::not_found("user not found"))?;
    gradient_db::users::managed_by_id(db.inner(), user_id)
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
