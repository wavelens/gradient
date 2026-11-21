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
use gradient_db::users::{ScimUserFilter, ValidatedNewUser};
use gradient_entity::user::{ActiveModel as UserActive, Model as UserModel};
use gradient_types::input::{choose_account_display_name_from_identity_provider, validate_email};
use gradient_types::{AccountOrigin, UserId};

use super::dto::*;
use super::error::{SCIM_CONTENT_TYPE, ScimError, ScimResult};
use super::filter::parse_eq_filter;
use crate::error::is_unique_violation;

fn map_user_database_error_to_scim_error(e: sea_orm::DbErr) -> ScimError {
    if is_unique_violation(&e) {
        ScimError::conflict("Username or email address already in use")
    } else {
        e.into()
    }
}

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
    description = "Call this endpoint to list SCIM-provisioned users. Other origins are the state file, registration, and external sign-in. Users from those origins don't appear here. This endpoint can apply a subset of filters from `userName eq \"...\"` and `externalId eq \"...\"`. This endpoint can also paginate with `startIndex`/`count`.",
    params(UserListQuery),
    responses((status = OK, description = "User list", body = super::dto::ListResponse<UserResource>, content_type = "application/scim+json"), ScimError)
)]
pub async fn list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<UserListQuery>,
) -> ScimResult<impl IntoResponse> {
    let db = state.web_db.inner();
    let filter = match q.filter.as_deref() {
        None => ScimUserFilter::Any,
        Some(f) => match parse_eq_filter(f) {
            Some((attr, val)) if attr == "username" => ScimUserFilter::Username(val),
            Some((attr, val)) if attr == "externalid" => ScimUserFilter::ScimExternalId(val),
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
    let total = usize::try_from(
        gradient_db::users::count_scim_provisioned_users_matching_filter(db, &filter).await?,
    )
    .unwrap_or(usize::MAX);
    let rows = if count == 0 {
        Vec::new()
    } else {
        gradient_db::users::list_scim_provisioned_users_page_matching_filter(
            db,
            &filter,
            start - 1,
            count,
        )
        .await?
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
    description = "Create a passwordless user for SCIM to own. Only accept that user on the first external sign-in if the provider claims include a verified email address equal to the user's email. Invalid `userName` values or email addresses return 400. `userName` conflicts return 409.",
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

    let new_user = ValidatedNewUser::new(
        AccountOrigin::ScimProvisionedByIdentityProvider,
        &body.user_name,
        &scim_user_display_name_from_request(&body),
        scim_user_request_primary_email(&body),
    )
    .map_err(|e| ScimError::bad_request("invalidValue", e.to_string()))?
    .with_email_verified(true)
    .with_active(body.active)
    .with_scim_external_id(body.external_id.clone());
    let u = gradient_db::users::insert_user_account(db, new_user)
        .await
        .map_err(map_user_database_error_to_scim_error)?;
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
    active.name = Set(scim_user_display_name_from_request(&body));
    if let Some(email) = scim_user_request_primary_email(&body) {
        validate_email(email).map_err(|e| ScimError::bad_request("invalidValue", e.to_string()))?;
        active.email = Set(email.to_owned());
    }

    active.active = Set(body.active);
    let u = gradient_db::users::update(state.web_db.inner(), active)
        .await
        .map_err(map_user_database_error_to_scim_error)?;
    Ok(scim_json(StatusCode::OK, UserResource::from(&u)))
}

#[utoipa::path(
    patch,
    path = "/Users/{id}",
    operation_id = "scimPatchUser",
    tag = "scim",
    summary = "Patch a SCIM user",
    description = "A SCIM PatchOp can carry `add` or `replace` operations. These operations apply to `active`, `displayName`, `name.formatted`. Other operations and attributes produce a 400 response and leave everything unchanged.",
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
    let username = u.username.clone();
    let mut active: UserActive = u.into();
    for op in &body.operations {
        if !matches!(op.op.to_ascii_lowercase().as_str(), "replace" | "add") {
            return Err(ScimError::unsupported_operation());
        }

        apply_user_patch(&mut active, op, &username)?;
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
    gradient_db::users::fetch_scim_provisioned_user_by_id(db.inner(), user_id)
        .await?
        .ok_or_else(|| ScimError::not_found("user not found"))
}

fn apply_user_patch(
    active: &mut UserActive,
    op: &PatchOperation,
    username: &str,
) -> ScimResult<()> {
    let path = op.path.as_deref().unwrap_or("").to_ascii_lowercase();
    let value = op.value.clone().unwrap_or(serde_json::Value::Null);
    match path.as_str() {
        "active" => {
            let v = value
                .as_bool()
                .ok_or_else(|| ScimError::bad_request("invalidValue", "active must be boolean"))?;
            active.active = Set(v);
        }
        "displayname" | "name.formatted" => active.name = Set(display_name(&value, username)?),
        "" => {
            let obj = value.as_object().ok_or_else(ScimError::unsupported_path)?;
            for (attribute, value) in obj {
                match attribute.as_str() {
                    "active" => {
                        let v = value.as_bool().ok_or_else(|| {
                            ScimError::bad_request("invalidValue", "active must be boolean")
                        })?;
                        active.active = Set(v);
                    }
                    "displayName" => active.name = Set(display_name(value, username)?),
                    _ => return Err(ScimError::unsupported_path()),
                }
            }
        }
        _ => return Err(ScimError::unsupported_path()),
    }

    Ok(())
}

fn scim_user_request_primary_email(body: &UserRequest) -> Option<&str> {
    body.emails
        .iter()
        .find(|e| e.primary)
        .or_else(|| body.emails.first())
        .map(|e| e.value.as_str())
}

fn scim_user_display_name_from_request(body: &UserRequest) -> String {
    let provided = body
        .display_name
        .as_deref()
        .or_else(|| body.name.as_ref().and_then(|n| n.formatted.as_deref()));
    choose_account_display_name_from_identity_provider(provided, &body.user_name)
}

fn display_name(value: &serde_json::Value, username: &str) -> ScimResult<String> {
    value
        .as_str()
        .map(|provided| {
            choose_account_display_name_from_identity_provider(Some(provided), username)
        })
        .ok_or_else(|| ScimError::bad_request("invalidValue", "Make `displayName` a string."))
}
