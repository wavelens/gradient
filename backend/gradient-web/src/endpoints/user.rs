/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, effective_cache_mask, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{MaybeApiKey, generate_api_key, hash_api_key};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::{
    PermissionEntry, available_cache_permissions, available_permissions, mask_to_vec,
    parse_cache_permission_list, parse_permission_list,
};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::{Extension, Json};

use chrono::Duration;
use gradient_core::ServerState;
use gradient_db::get_any_organization_by_name;
use gradient_types::consts::*;
use gradient_types::input::{validate_display_name, validate_username};
use gradient_types::*;
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
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub organization: Option<String>,
    /// Optional cache name to pin the key to. Mutually exclusive with `organization`.
    #[serde(default)]
    pub cache: Option<String>,
    /// CIDR strings the key may be used from. Empty or omitted = any source.
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
pub struct PatchApiKeyRequest {
    pub name: Option<String>,
    /// Wholesale replacement of the key's permission set. Omit to leave the
    /// existing mask alone.
    pub permissions: Option<Vec<String>>,
    /// Patch semantics for the org pin: omit to leave alone, `Some(name)` to
    /// pin, `Some(null)` (i.e. JSON null) to unpin.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub organization: Option<Option<String>>,
    /// Wholesale replacement; `[]` clears the allowlist.
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
}

fn deserialize_optional_field<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(de)?))
}

#[derive(Serialize, Debug)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub managed: bool,
    pub permissions: Vec<&'static str>,
    /// Org name (resolved from the pinned org id at response time), or `null`.
    pub organization: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    /// CIDR allowlist (canonicalized). Empty list = any source.
    pub allowed_ips: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct ApiKeyPermissionsResponse {
    pub available_permissions: Vec<PermissionEntry>,
    #[serde(rename = "availableCache")]
    pub available_cache: Vec<PermissionEntry>,
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
    info: RequestInfo,
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
    if dt == *NULL_TIME {
        None
    } else {
        Some(fmt_dt(dt))
    }
}

async fn resolve_org_pin(
    state: &Arc<ServerState>,
    user_id: UserId,
    name: Option<String>,
) -> WebResult<Option<OrganizationId>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let unknown = || {
        WebError::bad_request(format!(
            "Unknown organization or not a member: '{}'.",
            trimmed
        ))
    };
    let org = get_any_organization_by_name(&state.db(), trimmed.into())
        .await?
        .ok_or_else(unknown)?;
    let is_member = crate::access::is_org_member(state, user_id, org.id, None).await?;
    if !is_member {
        return Err(unknown());
    }
    Ok(Some(org.id))
}

fn forbid_via_api_key(api_key: &MaybeApiKey) -> WebResult<()> {
    if api_key.as_ref().is_some() {
        return Err(WebError::forbidden(
            "API keys cannot manage API keys. Use a session token.",
        ));
    }
    Ok(())
}

async fn org_name_lookup(
    state: &Arc<ServerState>,
    org_ids: &[OrganizationId],
) -> WebResult<std::collections::HashMap<OrganizationId, String>> {
    if org_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = EOrganization::find()
        .filter(COrganization::Id.is_in(org_ids.to_vec()))
        .all(&state.web_db)
        .await?;
    Ok(rows.into_iter().map(|o| (o.id, o.name)).collect())
}

fn api_key_info(
    key: gradient_entity::api::Model,
    org_names: &std::collections::HashMap<OrganizationId, String>,
) -> ApiKeyInfo {
    ApiKeyInfo {
        id: key.id.to_string(),
        name: key.name,
        managed: key.managed,
        permissions: mask_to_vec(key.permission)
            .into_iter()
            .map(|p| p.as_wire_name())
            .collect(),
        organization: key.organization.and_then(|id| org_names.get(&id).cloned()),
        created_at: fmt_dt(key.created_at),
        last_used_at: last_used_or_none(key.last_used_at),
        expires_at: fmt_opt_dt(key.expires_at),
        revoked_at: fmt_opt_dt(key.revoked_at),
        allowed_ips: key.allowed_ips.unwrap_or_default(),
    }
}

fn normalize_allowed_ips(raw: Option<Vec<String>>) -> Result<Option<Vec<String>>, WebError> {
    let Some(entries) = raw else { return Ok(None) };
    if entries.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let canon = crate::ip_allowlist::normalize_entry(&e).map_err(|err| {
            WebError::bad_request_with(
                crate::error::ErrorCode::INVALID_ALLOWED_IP,
                format!("invalid allowed_ips entry '{e}': {err}"),
            )
        })?;
        out.push(canon);
    }
    Ok(Some(out))
}

pub async fn get_key_permissions() -> WebResult<Json<BaseResponse<ApiKeyPermissionsResponse>>> {
    Ok(ok_json(ApiKeyPermissionsResponse {
        available_permissions: available_permissions(),
        available_cache: available_cache_permissions(),
    }))
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

    let pinned: Vec<OrganizationId> = api_keys.iter().filter_map(|k| k.organization).collect();
    let org_names = org_name_lookup(&state, &pinned).await?;

    let infos: Vec<ApiKeyInfo> = api_keys
        .into_iter()
        .map(|k| api_key_info(k, &org_names))
        .collect();

    Ok(ok_json(infos))
}

pub async fn post_keys(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Json(body): Json<CreateApiKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    forbid_via_api_key(&api_key_caller)?;

    if body.cache.is_some() && body.organization.is_some() {
        return Err(WebError::bad_request(
            "API key cannot pin to both an organization and a cache.",
        ));
    }

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
        .map(|days| gradient_types::now() + Duration::days(days as i64));

    let (mask, org_pin, cache_pin) = if let Some(cache_name) = body.cache.clone() {
        let cache = load_cache(
            &state,
            Caller::User(&user),
            None,
            cache_name,
            CacheAccess::Readable,
        )
        .await?;
        let m = parse_cache_permission_list(&body.permissions, "GET /user/keys/permissions")?;
        if m == 0 {
            return Err(WebError::bad_request(
                "At least one permission is required for an API key.",
            ));
        }
        let granted = effective_cache_mask(&state, user.id, cache.id, None)
            .await?
            .ok_or_else(|| WebError::not_found("Cache"))?;
        if m & !granted != 0 {
            return Err(WebError::forbidden(
                "API key cannot grant cache permissions you do not hold.",
            ));
        }
        (m, None, Some(cache.id))
    } else {
        let m = parse_permission_list(&body.permissions, "GET /user/keys/permissions")?;
        if m == 0 {
            return Err(WebError::bad_request(
                "At least one permission is required for an API key.",
            ));
        }
        let org = resolve_org_pin(&state, user.id, body.organization.clone()).await?;
        (m, org, None)
    };

    let allowed_ips = normalize_allowed_ips(body.allowed_ips.clone())?;
    let raw_key = generate_api_key();
    let api_key = MApi {
        id: ApiId::now_v7(),
        owned_by: user.id,
        name: body.name.clone(),
        key: hash_api_key(&raw_key),
        last_used_at: *NULL_TIME,
        created_at: gradient_types::now(),
        expires_at,
        permission: mask,
        organization: org_pin,
        cache: cache_pin,
        allowed_ips,
        ..Default::default()
    }
    .into_active_model();

    let inserted = api_key.insert(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::API_KEY_CREATE,
        &info,
        Some(serde_json::json!({
            "api_key_id": inserted.id.to_string(),
            "name": body.name,
            "expires_in_days": body.expires_in_days,
            "permissions_mask": mask,
            "organization_id": org_pin.map(|id| id.to_string()),
            "cache_id": cache_pin.map(|id| id.to_string()),
        })),
    )
    .await;

    Ok(Json(BaseResponse {
        error: false,
        message: format!("GRAD{}", raw_key),
    }))
}

pub async fn patch_key(
    state: State<Arc<ServerState>>,
    request_info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Path(api_id): Path<ApiId>,
    Json(body): Json<PatchApiKeyRequest>,
) -> WebResult<Json<BaseResponse<ApiKeyInfo>>> {
    forbid_via_api_key(&api_key_caller)?;

    let api_key = EApi::find_by_id(api_id)
        .one(&state.web_db)
        .await?
        .or_not_found("API-Key")?;
    if api_key.owned_by != user.id {
        return Err(WebError::not_found("API-Key"));
    }
    if api_key.managed {
        return Err(WebError::forbidden(
            "Cannot modify a state-managed API key.",
        ));
    }

    let previous_mask = api_key.permission;
    let previous_org = api_key.organization;
    let previous_name = api_key.name.clone();
    let mut active: AApi = api_key.into_active_model();

    if let Some(name) = body.name {
        if name != previous_name {
            let clash = EApi::find()
                .filter(
                    Condition::all()
                        .add(CApi::OwnedBy.eq(user.id))
                        .add(CApi::Name.eq(name.clone()))
                        .add(CApi::Id.ne(api_id)),
                )
                .one(&state.web_db)
                .await?;
            if clash.is_some() {
                return Err(WebError::already_exists("API-Key Name"));
            }
        }
        active.name = Set(name);
    }
    let mut new_mask = previous_mask;
    if let Some(perms) = body.permissions {
        new_mask = parse_permission_list(&perms, "GET /user/keys/permissions")?;
        if new_mask == 0 {
            return Err(WebError::bad_request(
                "At least one permission is required for an API key.",
            ));
        }
        active.permission = Set(new_mask);
    }
    let mut new_org = previous_org;
    if let Some(maybe_name) = body.organization {
        new_org = resolve_org_pin(&state, user.id, maybe_name).await?;
        active.organization = Set(new_org);
    }
    if let Some(canon) = normalize_allowed_ips(body.allowed_ips)? {
        active.allowed_ips = Set(if canon.is_empty() { None } else { Some(canon) });
    }

    let updated = active.update(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::API_KEY_UPDATE,
        &request_info,
        Some(serde_json::json!({
            "api_key_id": api_id.to_string(),
            "previous_name": previous_name,
            "new_name": updated.name,
            "previous_permissions_mask": previous_mask,
            "new_permissions_mask": new_mask,
            "previous_organization_id": previous_org.map(|i| i.to_string()),
            "new_organization_id": new_org.map(|i| i.to_string()),
        })),
    )
    .await;

    let pinned: Vec<OrganizationId> = updated.organization.iter().copied().collect();
    let org_names = org_name_lookup(&state, &pinned).await?;
    Ok(ok_json(api_key_info(updated, &org_names)))
}

pub async fn delete_keys(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Json(body): Json<ApiKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    forbid_via_api_key(&api_key_caller)?;
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
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Path(api_id): Path<ApiId>,
) -> WebResult<Json<BaseResponse<String>>> {
    forbid_via_api_key(&api_key_caller)?;
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
    active.revoked_at = Set(Some(gradient_types::now()));
    active.update(&state.web_db).await?;

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

    let now = gradient_types::now();
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
    info: RequestInfo,
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
    active.revoked_at = Set(Some(gradient_types::now()));
    active.update(&state.web_db).await?;

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
        return Err(WebError::forbidden(
            "Cannot modify state-managed user. This user is managed by configuration and cannot be edited through the API.",
        ));
    }

    // OIDC users cannot edit their profile - identity is managed by the provider
    if user.password.is_none() {
        return Err(WebError::forbidden(
            "Cannot modify profile of an OIDC user. Your profile is managed by your identity provider.",
        ));
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
