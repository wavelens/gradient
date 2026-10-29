/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, effective_cache_mask, load_cache};
use crate::audit::Audit;
use crate::authorization::{MaybeApiKey, generate_api_key, hash_api_key, session_claims};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, paginate};
use crate::json_body::JsonBody;
use crate::lookup::require;
use crate::names::UserDisplayName;
use crate::permissions::{
    PermissionEntry, available_cache_permissions, available_permissions, mask_to_vec,
    parse_cache_permission_list, parse_permission_list,
};
use crate::refusal::{Change, Refusal, RefusalStatus, SharedRefusal};
use crate::resource::Resource;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::{Extension, Json};
use gradient_db::lookup::{by_ids, get_any_project_by_name};
use gradient_db::{DbContext, WebDb};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;

use chrono::Duration;
use gradient_core::ServerState;
use gradient_types::consts::*;
use gradient_types::input::validate_username;
use gradient_types::*;
use password_auth::verify_password;
use sea_orm::ActiveValue::Set;
use sea_orm::IntoActiveModel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
enum UserRefusal {
    #[error("Password confirmation required")]
    PasswordConfirmationRequired,
    #[error("Password confirmation invalid")]
    PasswordConfirmationInvalid,
    #[error("Username confirmation required")]
    UsernameConfirmationRequired,
    #[error("Username confirmation does not match")]
    UsernameConfirmationMismatch,
    #[error("Unknown project or not a member: '{project}'.")]
    UnknownProjectOrNotMember { project: String },
    #[error("API keys cannot manage API keys. Use a session token.")]
    ApiKeyCannotManageApiKeys,
    #[error("API key cannot pin to both a project and a cache.")]
    PinToProjectAndCache,
    #[error("At least one permission is required for an API key.")]
    NoPermissionsSelected,
    #[error("API key cannot grant cache permissions you do not hold.")]
    GrantUnheldCachePermissions,
    #[error(
        "Cannot modify profile of an OIDC user. Your profile is managed by your identity provider."
    )]
    ModifyOidcUserProfile,
    #[error("Invalid version '{version}'.")]
    InvalidVersion { version: String },
}

impl Refusal for UserRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::PasswordConfirmationRequired
            | Self::PasswordConfirmationInvalid
            | Self::UsernameConfirmationRequired
            | Self::UsernameConfirmationMismatch
            | Self::ApiKeyCannotManageApiKeys
            | Self::GrantUnheldCachePermissions
            | Self::ModifyOidcUserProfile => RefusalStatus::Forbidden,
            Self::UnknownProjectOrNotMember { .. }
            | Self::PinToProjectAndCache
            | Self::NoPermissionsSelected
            | Self::InvalidVersion { .. } => RefusalStatus::BadRequest,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub superuser: bool,
    pub last_seen_version: Option<String>,
}

impl From<&MUser> for UserInfoResponse {
    fn from(user: &MUser) -> Self {
        Self {
            id: user.id.to_string(),
            username: user.username.clone(),
            name: user.name.clone(),
            email: user.email.clone(),
            superuser: user.superuser,
            last_seen_version: user.last_seen_version.clone(),
        }
    }
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct SeenVersionRequest {
    pub version: String,
}

fn is_version(version: &str) -> bool {
    (1..=64).contains(&version.len())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ApiKeyRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct CreateApiKeyRequest {
    pub name: String,
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub cache: Option<String>,
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchApiKeyRequest {
    pub name: Option<String>,
    pub permissions: Option<Vec<String>>,
    /// An omitted field leaves the pin unchanged. A name pins the key, and JSON null unpins it.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub project: Option<Option<String>>,
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

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub managed: bool,
    pub permissions: Vec<&'static str>,
    pub project: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub allowed_ips: Vec<String>,
}

impl ApiKeyInfo {
    fn new(key: MApi, project_names: &HashMap<ProjectId, String>) -> Self {
        Self {
            id: key.id.to_string(),
            name: key.name,
            managed: key.managed,
            permissions: mask_to_vec(key.permission)
                .into_iter()
                .map(|p| p.as_wire_name())
                .collect(),
            project: key.project.and_then(|id| project_names.get(&id).cloned()),
            created_at: fmt_dt(key.created_at),
            last_used_at: last_used_or_none(key.last_used_at),
            expires_at: fmt_opt_dt(key.expires_at),
            revoked_at: fmt_opt_dt(key.revoked_at),
            allowed_ips: key.allowed_ips.unwrap_or_default(),
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ApiKeyPermissionsResponse {
    pub available_permissions: Vec<PermissionEntry>,
    #[serde(rename = "availableCache")]
    pub available_cache: Vec<PermissionEntry>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct DeleteUserRequest {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub confirm_username: Option<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
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

impl SessionInfo {
    fn new(session: MSession, current_session: Option<SessionId>) -> Self {
        Self {
            id: session.id.to_string(),
            user_agent: session.user_agent,
            ip: session.ip,
            created_at: fmt_dt(session.created_at),
            last_used_at: fmt_dt(session.last_used_at),
            expires_at: fmt_dt(session.expires_at),
            remember_me: session.remember_me,
            current: Some(session.id) == current_session,
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct AuditLogEntry {
    pub id: String,
    pub event: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
}

impl From<MAuditLog> for AuditLogEntry {
    fn from(entry: MAuditLog) -> Self {
        Self {
            id: entry.id.to_string(),
            event: entry.event,
            ip: entry.ip,
            user_agent: entry.user_agent,
            metadata: entry.metadata,
            created_at: fmt_dt(entry.created_at),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchUserSettingsRequest {
    pub username: Option<String>,
    pub name: Option<UserDisplayName>,
    pub email: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct GetUserSettingsResponse {
    pub username: String,
    pub name: String,
    pub email: String,
    pub is_oidc: bool,
    pub managed: bool,
}

impl From<&MUser> for GetUserSettingsResponse {
    fn from(user: &MUser) -> Self {
        Self {
            is_oidc: user.password.is_none(),
            managed: user.managed,
            username: user.username.clone(),
            name: user.name.clone(),
            email: user.email.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct UserSearchResult {
    pub id: String,
    pub username: String,
    pub name: String,
}

impl From<MUser> for UserSearchResult {
    fn from(user: MUser) -> Self {
        Self {
            id: user.id.to_string(),
            username: user.username,
            name: user.name,
        }
    }
}

#[utoipa::path(
    get,
    path = "/user/search",
    operation_id = "searchUsers",
    tag = "user",
    summary = "Search users by username",
    description = "Returns up to 10 users whose username contains the given query string. Used for autocomplete in member and ownership dialogs.",
    params(("q" = String, Query)),
    responses((status = OK, description = "Matching users", body = Vec<UserSearchResult>), WebError)
)]
pub async fn get_search(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<Vec<UserSearchResult>>>> {
    let q = params.get("q").cloned().unwrap_or_default();

    let users = gradient_db::users::search_by_username(&state.web_db, &q, 10).await?;

    let results = users.into_iter().map(UserSearchResult::from).collect();

    Ok(ok_json(results))
}

#[utoipa::path(
    get,
    path = "/user",
    operation_id = "getUser",
    tag = "user",
    summary = "Get current user",
    description = "Returns the id, username, display name, and email of the authenticated user.",
    responses((status = OK, description = "User info", body = UserInfoResponse), WebError)
)]
pub async fn get(
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<UserInfoResponse>>> {
    let user_info = UserInfoResponse::from(&user);

    let res = BaseResponse {
        error: false,
        message: user_info,
    };

    Ok(Json(res))
}

#[utoipa::path(
    put,
    path = "/user/whats-new",
    operation_id = "putUserSeenVersion",
    tag = "user",
    summary = "Mark release notes as seen",
    description = "Stores the version whose release notes the user saw last. The web UI shows the notes of every newer version once and sends the version it runs here when the user closes them. `last_seen_version` of `GET /user` holds the value and it is empty for an account that never saw any notes.",
    request_body = SeenVersionRequest,
    responses((status = OK, description = "Version stored", body = String, content_type = "application/json"), WebError)
)]
pub async fn put_seen_version(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    JsonBody(body): JsonBody<SeenVersionRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !is_version(&body.version) {
        return Err(UserRefusal::InvalidVersion {
            version: body.version,
        }
        .into());
    }

    let mut auser: AUser = user.into();
    auser.last_seen_version = Set(Some(body.version));
    gradient_db::rows::update::<EUser, _>(&state.web_db, auser).await?;

    Ok(ok_json("Version stored".to_string()))
}

#[utoipa::path(
    delete,
    path = "/user",
    operation_id = "deleteUser",
    tag = "user",
    summary = "Delete current user",
    description = "Permanently deletes the authenticated user and all associated data.\nRequires re-authentication: password-auth users must supply their\ncurrent password in `password`; OIDC-auth users must echo their\nusername in `confirm_username`.",
    request_body = DeleteUserRequest,
    responses((status = OK, description = "User deleted", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Json(body): Json<DeleteUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if let Some(ref hashed) = user.password {
        let provided = body
            .password
            .as_ref()
            .ok_or(UserRefusal::PasswordConfirmationRequired)?;
        verify_password(provided, hashed).map_err(|_| UserRefusal::PasswordConfirmationInvalid)?;
    } else {
        let confirm = body
            .confirm_username
            .as_ref()
            .ok_or(UserRefusal::UsernameConfirmationRequired)?;
        if confirm != &user.username {
            return Err(UserRefusal::UsernameConfirmationMismatch.into());
        }
    }

    gradient_db::rows::delete::<EUser, _>(&state.web_db, user).await?;

    audit
        .write(Action::UserDelete, EventOwner::default(), None)
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

async fn resolve_project_pin(
    db: &DbContext,
    user_id: UserId,
    name: Option<String>,
) -> WebResult<Option<ProjectId>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let unknown = || UserRefusal::UnknownProjectOrNotMember {
        project: trimmed.to_string(),
    };
    let project = get_any_project_by_name(db, trimmed.into())
        .await?
        .ok_or_else(unknown)?;
    let is_member = crate::access::is_project_member(&db.web_db, user_id, project.id, None).await?;
    if !is_member {
        return Err(unknown().into());
    }
    Ok(Some(project.id))
}

pub(crate) fn forbid_via_api_key(api_key: &MaybeApiKey) -> WebResult<()> {
    if api_key.as_ref().is_some() {
        return Err(UserRefusal::ApiKeyCannotManageApiKeys.into());
    }
    Ok(())
}

async fn project_name_lookup(
    db: &WebDb,
    project_ids: &[ProjectId],
) -> WebResult<HashMap<ProjectId, String>> {
    Ok(by_ids::<EProject, _>(db, project_ids)
        .await?
        .into_iter()
        .map(|(id, project)| (id, project.name))
        .collect())
}

fn normalize_allowed_ips(raw: Option<Vec<String>>) -> Result<Option<Vec<String>>, WebError> {
    let Some(entries) = raw else { return Ok(None) };
    if entries.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let canon = crate::ip_allowlist::normalize_entry(&e).map_err(|err| {
            SharedRefusal::InvalidAllowedIp {
                entry: e.to_string(),
                reason: err.to_string(),
            }
        })?;
        out.push(canon);
    }
    Ok(Some(out))
}

#[utoipa::path(
    get,
    path = "/user/keys/permissions",
    operation_id = "getUserKeyPermissions",
    tag = "user",
    summary = "List API-key permission catalogue",
    description = "Returns the full set of capabilities a user-managed API key may grant.\nUsed by the create / edit dialog to populate the permission checkbox list.",
    responses((status = OK, description = "Permission catalogue", body = ApiKeyPermissionsResponse), WebError)
)]
pub async fn get_key_permissions() -> WebResult<Json<BaseResponse<ApiKeyPermissionsResponse>>> {
    Ok(ok_json(ApiKeyPermissionsResponse {
        available_permissions: available_permissions(),
        available_cache: available_cache_permissions(),
    }))
}

#[utoipa::path(
    get,
    path = "/user/keys",
    operation_id = "getUserKeys",
    tag = "user",
    summary = "List API keys",
    description = "Returns all API keys belonging to the authenticated user, including\ncreation, last-used, expiry, and revocation timestamps.",
    responses((status = OK, description = "List of API keys", body = Vec<ApiKeyInfo>), WebError)
)]
pub async fn get_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<ApiKeyInfo>>>> {
    let api_keys = gradient_db::api_keys::of_user(&state.web_db, user.id).await?;

    let pinned: Vec<ProjectId> = api_keys.iter().filter_map(|k| k.project).collect();
    let project_names = project_name_lookup(&state.web_db, &pinned).await?;

    let infos: Vec<ApiKeyInfo> = api_keys
        .into_iter()
        .map(|k| ApiKeyInfo::new(k, &project_names))
        .collect();

    Ok(ok_json(infos))
}

#[utoipa::path(
    post,
    path = "/user/keys",
    operation_id = "createUserKey",
    tag = "user",
    summary = "Create API key",
    description = "Creates a new API key. The full key (prefixed with `GRAD`) is returned once -\nit cannot be retrieved again. Use it as `Authorization: Bearer GRAD<key>`.\nPass `expires_in_days` to make the key auto-expire; omit for no expiry.\nA cache-pinned key may only grant cache permissions the caller already\nholds; members of a project subscribed to the cache are treated as\nread-only, so they can mint read-only cache keys.",
    request_body = CreateApiKeyRequest,
    responses((status = OK, description = "New API key", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_keys(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Json(body): Json<CreateApiKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let db = state.db();
    forbid_via_api_key(&api_key_caller)?;

    if body.cache.is_some() && body.project.is_some() {
        return Err(UserRefusal::PinToProjectAndCache.into());
    }

    let existing_api_key =
        gradient_db::api_keys::by_owner_and_name(&state.web_db, user.id, &body.name).await?;
    if existing_api_key.is_some() {
        return Err(WebError::already_exists(Resource::ApiKeyName));
    }

    let expires_at = body
        .expires_in_days
        .map(|days| gradient_types::now() + Duration::days(days as i64));

    let (mask, project_pin, cache_pin) = if let Some(cache_name) = body.cache.clone() {
        let cache = load_cache(
            &db,
            Caller::User(&user),
            None,
            cache_name,
            CacheAccess::Readable,
        )
        .await?;
        let m = parse_cache_permission_list(&body.permissions, "GET /user/keys/permissions")?;
        if m == 0 {
            return Err(UserRefusal::NoPermissionsSelected.into());
        }
        let granted = effective_cache_mask(&state.web_db, user.id, cache.id, None)
            .await?
            .ok_or_else(|| WebError::not_found(Resource::Cache))?;
        if m & !granted != 0 {
            return Err(UserRefusal::GrantUnheldCachePermissions.into());
        }
        (m, None, Some(cache.id))
    } else {
        let m = parse_permission_list(&body.permissions, "GET /user/keys/permissions")?;
        if m == 0 {
            return Err(UserRefusal::NoPermissionsSelected.into());
        }
        let project = resolve_project_pin(&db, user.id, body.project.clone()).await?;
        (m, project, None)
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
        project: project_pin,
        cache: cache_pin,
        allowed_ips,
        ..Default::default()
    };

    let inserted = gradient_db::rows::insert::<EApi, _>(&state.web_db, api_key)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::ApiKeyName))?;

    audit.write(Action::ApiKeyCreate, EventOwner::default(), serde_json::json!({ "api_key_id": inserted.id.to_string(), "name": body.name, "expires_in_days": body.expires_in_days, "permissions_mask": mask, "project_id": project_pin.map(|id| id.to_string()), "cache_id": cache_pin.map(|id| id.to_string()), }))
    .await;

    Ok(Json(BaseResponse {
        error: false,
        message: format!("GRAD{}", raw_key),
    }))
}

#[utoipa::path(
    patch,
    path = "/user/keys/{api_id}",
    operation_id = "patchUserKey",
    tag = "user",
    summary = "Update an API key's configurable options",
    description = "Edit the key's name, permission set, or project pin in place.\nPermissions replace the existing set wholesale. State-managed keys\ncannot be edited. Cannot be called from an API-key-authenticated\nrequest - sessions only.",
    params(("api_id" = ApiId, Path)),
    request_body = PatchApiKeyRequest,
    responses((status = OK, description = "Updated key", body = ApiKeyInfo), WebError)
)]
pub async fn patch_key(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Path(api_id): Path<ApiId>,
    Json(body): Json<PatchApiKeyRequest>,
) -> WebResult<Json<BaseResponse<ApiKeyInfo>>> {
    let db = state.db();
    forbid_via_api_key(&api_key_caller)?;

    let api_key = require::<EApi>(&state.web_db, api_id).await?;
    if api_key.owned_by != user.id {
        return Err(WebError::not_found(Resource::ApiKey));
    }
    if api_key.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::ApiKey).into());
    }

    let previous_mask = api_key.permission;
    let previous_project = api_key.project;
    let previous_name = api_key.name.clone();
    let mut active: AApi = api_key.into_active_model();

    if let Some(name) = body.name {
        if name != previous_name {
            let clash =
                gradient_db::api_keys::other_with_name(&state.web_db, user.id, &name, api_id)
                    .await?;
            if clash.is_some() {
                return Err(WebError::already_exists(Resource::ApiKeyName));
            }
        }
        active.name = Set(name);
    }
    let mut new_mask = previous_mask;
    if let Some(perms) = body.permissions {
        new_mask = parse_permission_list(&perms, "GET /user/keys/permissions")?;
        if new_mask == 0 {
            return Err(UserRefusal::NoPermissionsSelected.into());
        }
        active.permission = Set(new_mask);
    }
    let mut new_project = previous_project;
    if let Some(maybe_name) = body.project {
        new_project = resolve_project_pin(&db, user.id, maybe_name).await?;
        active.project = Set(new_project);
    }
    if let Some(canon) = normalize_allowed_ips(body.allowed_ips)? {
        active.allowed_ips = Set(if canon.is_empty() { None } else { Some(canon) });
    }

    let updated = gradient_db::rows::update::<EApi, _>(&state.web_db, active)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::ApiKeyName))?;

    audit.write(Action::ApiKeyUpdate, EventOwner::default(), serde_json::json!({ "api_key_id": api_id.to_string(), "previous_name": previous_name, "new_name": updated.name, "previous_permissions_mask": previous_mask, "new_permissions_mask": new_mask, "previous_project_id": previous_project.map(|i| i.to_string()), "new_project_id": new_project.map(|i| i.to_string()), }))
    .await;

    let pinned: Vec<ProjectId> = updated.project.iter().copied().collect();
    let project_names = project_name_lookup(&state.web_db, &pinned).await?;
    Ok(ok_json(ApiKeyInfo::new(updated, &project_names)))
}

#[utoipa::path(
    delete,
    path = "/user/keys",
    operation_id = "deleteUserKey",
    tag = "user",
    summary = "Delete API key",
    description = "Deletes the API key with the given name. State-managed keys cannot be deleted.",
    request_body = ApiKeyRequest,
    responses((status = OK, description = "Key deleted", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_keys(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Json(body): Json<ApiKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    forbid_via_api_key(&api_key_caller)?;
    let api_key = gradient_db::api_keys::by_owner_and_name(&state.web_db, user.id, &body.name)
        .await?
        .or_not_found(Resource::ApiKey)?;

    if api_key.managed {
        return Err(SharedRefusal::StateManaged(Change::Delete, Resource::ApiKey).into());
    }

    let api_key_id = api_key.id;
    gradient_db::rows::delete::<EApi, _>(&state.web_db, api_key).await?;

    audit
        .write(
            Action::ApiKeyDelete,
            EventOwner::default(),
            serde_json::json!({ "api_key_id": api_key_id.to_string(), "name": body.name, }),
        )
        .await;

    let res = BaseResponse {
        error: false,
        message: "API-Key deleted".to_string(),
    };

    Ok(Json(res))
}

#[utoipa::path(
    post,
    path = "/user/keys/{api_id}/revoke",
    operation_id = "revokeUserKey",
    tag = "user",
    summary = "Revoke an API key",
    description = "Marks the key as revoked without deleting the row, so audit trails are\npreserved. A revoked key fails authentication immediately.",
    params(("api_id" = ApiId, Path)),
    responses((status = OK, description = "Key revoked", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_key_revoke(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key_caller): Extension<MaybeApiKey>,
    Path(api_id): Path<ApiId>,
) -> WebResult<Json<BaseResponse<String>>> {
    forbid_via_api_key(&api_key_caller)?;
    let api_key = require::<EApi>(&state.web_db, api_id).await?;

    if api_key.owned_by != user.id {
        return Err(WebError::not_found(Resource::ApiKey));
    }
    if api_key.managed {
        return Err(SharedRefusal::StateManaged(Change::Revoke, Resource::ApiKey).into());
    }
    if api_key.revoked_at.is_some() {
        return Ok(ok_json("API-Key already revoked".to_string()));
    }

    gradient_db::api_keys::revoke(&state.web_db, api_key, gradient_types::now()).await?;

    audit
        .write(
            Action::ApiKeyRevoke,
            EventOwner::default(),
            serde_json::json!({ "api_key_id": api_id.to_string() }),
        )
        .await;

    Ok(ok_json("API-Key revoked".to_string()))
}

#[utoipa::path(
    get,
    path = "/user/sessions",
    operation_id = "getUserSessions",
    tag = "user",
    summary = "List active sessions",
    description = "Returns the caller's non-revoked, non-expired sessions. Each entry\ncarries the user-agent and IP captured at login plus a `current` flag\nidentifying the session backing this request.",
    responses((status = OK, description = "Active sessions", body = Vec<SessionInfo>), WebError)
)]
pub async fn get_sessions(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<SessionInfo>>>> {
    let current_session_id = session_claims(&state.jwt_secret, &headers).map(|claims| claims.jti);

    let sessions = gradient_db::sessions::unrevoked_of_user(&state.web_db, user.id).await?;

    let now = gradient_types::now();
    let sessions: Vec<SessionInfo> = sessions
        .into_iter()
        .filter(|s| s.expires_at >= now)
        .map(|s| SessionInfo::new(s, current_session_id))
        .collect();

    Ok(ok_json(sessions))
}

#[utoipa::path(
    delete,
    path = "/user/sessions/{session_id}",
    operation_id = "deleteUserSession",
    tag = "user",
    summary = "Revoke a session",
    description = "Revokes a single session, immediately invalidating its JWT. Useful for\nsigning out a lost device without changing the password.",
    params(("session_id" = SessionId, Path)),
    responses((status = OK, description = "Session revoked", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_session(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Path(session_id): Path<SessionId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let session = require::<ESession>(&state.web_db, session_id).await?;

    if session.user_id != user.id {
        return Err(WebError::not_found(Resource::Session));
    }
    if session.revoked_at.is_some() {
        return Ok(ok_json("Session already revoked".to_string()));
    }

    gradient_db::sessions::revoke(&state.web_db, session, gradient_types::now()).await?;

    audit
        .write(
            Action::SessionRevoke,
            EventOwner::default(),
            serde_json::json!({ "session_id": session_id.to_string() }),
        )
        .await;

    Ok(ok_json("Session revoked".to_string()))
}

type PaginatedAuditLogEntry = Paginated<AuditLogEntry>;

#[utoipa::path(
    get,
    path = "/user/audit-log",
    operation_id = "getUserAuditLog",
    tag = "user",
    summary = "Audit log for the current user",
    description = "Paginated list of audit events recorded for the caller (login,\nlogout, API-key lifecycle, session revocation, account deletion).",
    params(PaginationParams),
    responses((status = OK, description = "Audit entries", body = PaginatedAuditLogEntry), WebError)
)]
pub async fn get_audit_log(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<AuditLogEntry>>>> {
    let listing = paginate(
        gradient_db::audit_log::of_user(user.id),
        &state.web_db,
        &params,
    )
    .await?
    .map(AuditLogEntry::from);

    Ok(ok_json(listing))
}

#[utoipa::path(
    get,
    path = "/user/settings",
    operation_id = "getUserSettings",
    tag = "user",
    summary = "Get user settings",
    description = "Returns the username, display name, and email of the current user.",
    responses((status = OK, description = "User settings", body = GetUserSettingsResponse), WebError)
)]
pub async fn get_settings(
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<GetUserSettingsResponse>>> {
    let res = BaseResponse {
        error: false,
        message: GetUserSettingsResponse::from(&user),
    };

    Ok(Json(res))
}

#[utoipa::path(
    patch,
    path = "/user/settings",
    operation_id = "patchUserSettings",
    tag = "user",
    summary = "Update user settings",
    description = "Updates one or more user profile fields. Only fields that are present in the\nrequest body are changed. Managed users (created via NixOS config) cannot be\nmodified through the API.",
    request_body = PatchUserSettingsRequest,
    responses((status = OK, description = "Settings updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_settings(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    JsonBody(body): JsonBody<PatchUserSettingsRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if user.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::User).into());
    }

    if user.password.is_none() {
        return Err(UserRefusal::ModifyOidcUserProfile.into());
    }

    let mut auser: AUser = user.into();

    if let Some(username) = body.username {
        if let Err(e) = validate_username(&username) {
            return Err(WebError::invalid_username(e.to_string()));
        }

        let existing_user = gradient_db::users::by_username(&state.web_db, &username).await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists(Resource::Username));
        }

        auser.username = Set(username);
    }

    if let Some(name) = body.name {
        auser.name = Set(name.into_inner());
    }

    if let Some(email) = body.email {
        let existing_user = gradient_db::users::by_email(&state.web_db, &email).await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists(Resource::Email));
        }

        auser.email = Set(email);
    }

    gradient_db::rows::update::<EUser, _>(&state.web_db, auser)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::Username))?;

    let res = BaseResponse {
        error: false,
        message: "User updated".to_string(),
    };

    Ok(Json(res))
}

#[cfg(test)]
mod tests {
    use super::is_version;

    #[test]
    fn seen_versions_are_release_names_only() {
        assert!(is_version("2.1.0"));
        assert!(is_version("2.1.0-rc1+build.5"));
        assert!(!is_version(""));
        assert!(!is_version("2.1.0 <b>"));
        assert!(!is_version(&"1".repeat(65)));
    }
}
