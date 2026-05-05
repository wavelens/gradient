/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{generate_api_key, hash_api_key};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::{Extension, Json};

use chrono::Duration;
use gradient_core::types::consts::*;
use gradient_core::types::input::{validate_display_name, validate_username};
use gradient_core::types::*;
use password_auth::verify_password;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub superuser: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ApiKeyRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CreateApiKeyRequest {
    pub name: String,
    /// Lifetime in days. `None` means the key never expires (legacy behaviour).
    #[serde(default)]
    pub expires_in_days: Option<u32>,
}

#[derive(Serialize, Debug)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub managed: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DeleteUserRequest {
    /// Password for password-auth users.
    #[serde(default)]
    pub password: Option<String>,
    /// Username confirmation for OIDC users (must equal the caller's username).
    #[serde(default)]
    pub confirm_username: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: String,
    pub last_used_at: String,
    pub expires_at: String,
    pub remember_me: bool,
    pub current: bool,
}

#[derive(Serialize, Debug)]
pub struct AuditLogEntry {
    pub id: String,
    pub event: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchUserSettingsRequest {
    pub username: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetUserSettingsResponse {
    pub username: String,
    pub name: String,
    pub email: String,
    pub is_oidc: bool,
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UserSearchResult {
    pub id: String,
    pub username: String,
    pub name: String,
}

pub async fn get_search(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<Vec<UserSearchResult>>>> {
    let q = params.get("q").cloned().unwrap_or_default();

    let users = EUser::find()
        .filter(CUser::Username.contains(q.as_str()))
        .limit(10)
        .all(&state.web_db)
        .await?;

    let results = users
        .into_iter()
        .map(|u| UserSearchResult {
            id: u.id.to_string(),
            username: u.username,
            name: u.name,
        })
        .collect();

    Ok(ok_json(results))
}

pub async fn get(
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<UserInfoResponse>>> {
    let user_info = UserInfoResponse {
        id: user.id.to_string(),
        username: user.username.clone(),
        name: user.name.clone(),
        email: user.email.clone(),
        superuser: user.superuser,
    };

    let res = BaseResponse {
        error: false,
        message: user_info,
    };

    Ok(Json(res))
}

pub async fn delete(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
    Json(body): Json<DeleteUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if let Some(ref hashed) = user.password {
        let provided = body
            .password
            .as_ref()
            .ok_or_else(|| WebError::forbidden("Password confirmation required"))?;
        verify_password(provided, hashed)
            .map_err(|_| WebError::forbidden("Password confirmation invalid"))?;
    } else {
        let confirm = body
            .confirm_username
            .as_ref()
            .ok_or_else(|| WebError::forbidden("Username confirmation required"))?;
        if confirm != &user.username {
            return Err(WebError::forbidden("Username confirmation does not match"));
        }
    }

    let info = RequestInfo::from_headers(&headers);
    let user_id = user.id;
    let auser: AUser = user.into();
    auser.delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user_id),
        events::USER_DELETE,
        &info,
        None,
    )
    .await;

    let res = BaseResponse {
        error: false,
        message: "User deleted".to_string(),
    };

    Ok(Json(res))
}

fn fmt_dt(dt: chrono::NaiveDateTime) -> String {
    dt.and_utc().to_rfc3339()
}

fn fmt_opt_dt(dt: Option<chrono::NaiveDateTime>) -> Option<String> {
    dt.map(fmt_dt)
}

fn last_used_or_none(dt: chrono::NaiveDateTime) -> Option<String> {
    if dt == *NULL_TIME { None } else { Some(fmt_dt(dt)) }
}

pub async fn get_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<ApiKeyInfo>>>> {
    let api_keys = EApi::find()
        .filter(CApi::OwnedBy.eq(user.id))
        .order_by_desc(CApi::CreatedAt)
        .all(&state.web_db)
        .await?;

    let api_keys: Vec<ApiKeyInfo> = api_keys
        .into_iter()
        .map(|k| ApiKeyInfo {
            id: k.id.to_string(),
            name: k.name,
            managed: k.managed,
            created_at: fmt_dt(k.created_at),
            last_used_at: last_used_or_none(k.last_used_at),
            expires_at: fmt_opt_dt(k.expires_at),
            revoked_at: fmt_opt_dt(k.revoked_at),
        })
        .collect();

    Ok(ok_json(api_keys))
}

pub async fn post_keys(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
    Json(body): Json<CreateApiKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let existing_api_key = EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(user.id))
                .add(CApi::Name.eq(body.name.clone())),
        )
        .one(&state.web_db)
        .await?;

    if existing_api_key.is_some() {
        return Err(WebError::already_exists("API-Key Name"));
    }

    let expires_at = body
        .expires_in_days
        .map(|days| gradient_core::types::now() + Duration::days(days as i64));

    let raw_key = generate_api_key();
    let api_key = AApi {
        id: Set(ApiId::now_v7()),
        owned_by: Set(user.id),
        name: Set(body.name.clone()),
        key: Set(hash_api_key(&raw_key)),
        last_used_at: Set(*NULL_TIME),
        created_at: Set(gradient_core::types::now()),
        managed: Set(false),
        expires_at: Set(expires_at),
        revoked_at: Set(None),
    };

    let inserted = api_key.insert(&state.web_db).await?;

    let info = RequestInfo::from_headers(&headers);
    audit_record(
        &state.web_db,
        Some(user.id),
        events::API_KEY_CREATE,
        &info,
        Some(serde_json::json!({
            "api_key_id": inserted.id.to_string(),
            "name": body.name,
            "expires_in_days": body.expires_in_days,
        })),
    )
    .await;

    let res = BaseResponse {
        error: false,
        message: format!("GRAD{}", raw_key),
    };

    Ok(Json(res))
}

pub async fn delete_keys(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
    Json(body): Json<ApiKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key = EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(user.id))
                .add(CApi::Name.eq(body.name.clone())),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("API-Key")?;

    if api_key.managed {
        return Err(WebError::forbidden(
            "Cannot delete a state-managed API key.".to_string(),
        ));
    }

    let api_key_id = api_key.id;
    let aapi_key: AApi = api_key.into();
    aapi_key.delete(&state.web_db).await?;

    let info = RequestInfo::from_headers(&headers);
    audit_record(
        &state.web_db,
        Some(user.id),
        events::API_KEY_DELETE,
        &info,
        Some(serde_json::json!({
            "api_key_id": api_key_id.to_string(),
            "name": body.name,
        })),
    )
    .await;

    let res = BaseResponse {
        error: false,
        message: "API-Key deleted".to_string(),
    };

    Ok(Json(res))
}

/// Revoke a single API key by id without deleting it (keeps the audit trail).
pub async fn post_key_revoke(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
    Path(api_id): Path<ApiId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key = EApi::find_by_id(api_id)
        .one(&state.web_db)
        .await?
        .or_not_found("API-Key")?;

    if api_key.owned_by != user.id {
        return Err(WebError::not_found("API-Key"));
    }
    if api_key.managed {
        return Err(WebError::forbidden(
            "Cannot revoke a state-managed API key.".to_string(),
        ));
    }
    if api_key.revoked_at.is_some() {
        return Ok(ok_json("API-Key already revoked".to_string()));
    }

    let mut active: AApi = api_key.into_active_model();
    active.revoked_at = Set(Some(gradient_core::types::now()));
    active.update(&state.web_db).await?;

    let info = RequestInfo::from_headers(&headers);
    audit_record(
        &state.web_db,
        Some(user.id),
        events::API_KEY_REVOKE,
        &info,
        Some(serde_json::json!({ "api_key_id": api_id.to_string() })),
    )
    .await;

    Ok(ok_json("API-Key revoked".to_string()))
}

pub async fn get_sessions(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<SessionInfo>>>> {
    let current_session_id = current_session_id_from_headers(&state, &headers);

    let sessions = ESession::find()
        .filter(CSession::UserId.eq(user.id))
        .filter(CSession::RevokedAt.is_null())
        .order_by_desc(CSession::LastUsedAt)
        .all(&state.web_db)
        .await?;

    let now = gradient_core::types::now();
    let sessions: Vec<SessionInfo> = sessions
        .into_iter()
        .filter(|s| s.expires_at >= now)
        .map(|s| SessionInfo {
            id: s.id.to_string(),
            user_agent: s.user_agent,
            ip: s.ip,
            created_at: fmt_dt(s.created_at),
            last_used_at: fmt_dt(s.last_used_at),
            expires_at: fmt_dt(s.expires_at),
            remember_me: s.remember_me,
            current: Some(s.id) == current_session_id,
        })
        .collect();

    Ok(ok_json(sessions))
}

pub async fn delete_session(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
    Path(session_id): Path<SessionId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let session = ESession::find_by_id(session_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Session")?;

    if session.user_id != user.id {
        return Err(WebError::not_found("Session"));
    }
    if session.revoked_at.is_some() {
        return Ok(ok_json("Session already revoked".to_string()));
    }

    let mut active: ASession = session.into_active_model();
    active.revoked_at = Set(Some(gradient_core::types::now()));
    active.update(&state.web_db).await?;

    let info = RequestInfo::from_headers(&headers);
    audit_record(
        &state.web_db,
        Some(user.id),
        events::SESSION_REVOKE,
        &info,
        Some(serde_json::json!({ "session_id": session_id.to_string() })),
    )
    .await;

    Ok(ok_json("Session revoked".to_string()))
}

pub async fn get_audit_log(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<AuditLogEntry>>>>> {
    let page = params.page();
    let per_page = params.per_page();
    let offset = (page - 1) * per_page;

    let total = EAuditLog::find()
        .filter(CAuditLog::UserId.eq(user.id))
        .count(&state.web_db)
        .await?;

    let rows = EAuditLog::find()
        .filter(CAuditLog::UserId.eq(user.id))
        .order_by_desc(CAuditLog::CreatedAt)
        .limit(per_page)
        .offset(offset)
        .all(&state.web_db)
        .await?;

    let items: Vec<AuditLogEntry> = rows
        .into_iter()
        .map(|r| AuditLogEntry {
            id: r.id.to_string(),
            event: r.event,
            ip: r.ip,
            user_agent: r.user_agent,
            metadata: r.metadata,
            created_at: fmt_dt(r.created_at),
        })
        .collect();

    Ok(ok_json(Paginated {
        items,
        total,
        page,
        per_page,
    }))
}

fn current_session_id_from_headers(
    state: &Arc<ServerState>,
    headers: &HeaderMap,
) -> Option<SessionId> {
    let token = crate::authorization::extract_bearer_or_cookie(headers)?;
    if token.starts_with("GRAD") {
        return None;
    }
    let data = jsonwebtoken::decode::<crate::authorization::Cliams>(
        &token,
        &jsonwebtoken::DecodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .ok()?;
    Some(data.claims.jti)
}

pub async fn get_settings(
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<GetUserSettingsResponse>>> {
    let res = BaseResponse {
        error: false,
        message: GetUserSettingsResponse {
            is_oidc: user.password.is_none(),
            managed: user.managed,
            username: user.username.clone(),
            name: user.name.clone(),
            email: user.email.clone(),
        },
    };

    Ok(Json(res))
}

pub async fn patch_settings(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<PatchUserSettingsRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    // Prevent modification of state-managed users
    if user.managed {
        return Err(WebError::forbidden("Cannot modify state-managed user. This user is managed by configuration and cannot be edited through the API."));
    }

    // OIDC users cannot edit their profile — identity is managed by the provider
    if user.password.is_none() {
        return Err(WebError::forbidden("Cannot modify profile of an OIDC user. Your profile is managed by your identity provider."));
    }

    let mut auser: AUser = user.into();

    if let Some(username) = body.username {
        if let Err(e) = validate_username(&username) {
            return Err(WebError::invalid_username(e.to_string()));
        }

        let existing_user = EUser::find()
            .filter(CUser::Username.eq(username.clone()))
            .one(&state.web_db)
            .await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists("Username"));
        }

        auser.username = Set(username);
    }

    if let Some(name) = body.name {
        if let Err(e) = validate_display_name(&name) {
            return Err(WebError::bad_request(format!("Invalid name: {}", e)));
        }
        auser.name = Set(name);
    }

    if let Some(email) = body.email {
        let existing_user = EUser::find()
            .filter(CUser::Email.eq(email.clone()))
            .one(&state.web_db)
            .await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists("Email"));
        }

        auser.email = Set(email);
    }

    auser.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "User updated".to_string(),
    };

    Ok(Json(res))
}
