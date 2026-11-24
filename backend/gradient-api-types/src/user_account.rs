/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub superuser: bool,
    pub last_seen_version: Option<String>,
    pub can_create_cache: bool,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ApiKeyRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct CreateApiKeyRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1)]
    pub expires_in_days: Option<u32>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub cache_permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_ips: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct PatchApiKeyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_permissions: Option<Vec<String>>,
    /// An omitted field leaves the pin unchanged. A name pins the key and JSON null unpins it.
    #[serde(
        default,
        deserialize_with = "crate::deserialize_optional_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub project: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "crate::deserialize_optional_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_ips: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema, Clone)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub managed: bool,
    pub permissions: Vec<String>,
    pub cache_permissions: Vec<String>,
    pub project: Option<String>,
    pub cache: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub allowed_ips: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ApiKeyPermissionsResponse {
    pub available_permissions: Vec<PermissionEntry>,
    #[serde(rename = "availableCache")]
    pub available_cache: Vec<PermissionEntry>,
    pub server_wide_permissions: Vec<PermissionEntry>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct DeleteUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_username: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct SessionInfo {
    pub id: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: String,
    pub last_used_at: String,
    pub expires_at: String,
    pub remember_me: bool,
    pub current: bool,
    /// True if `gradient login` created the session through approval in the browser. Such sessions end 30 days after sign-in and can't change account credentials.
    pub is_cli_device_sign_in_session: bool,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct AuditLogEntry {
    pub id: String,
    pub event: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchUserSettingsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<UserDisplayName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct GetUserSettingsResponse {
    pub username: String,
    pub name: String,
    pub email: String,
    pub is_oidc: bool,
    pub has_password: bool,
    pub login_providers: Vec<ConnectedLoginProvider>,
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ConnectedLoginProvider {
    pub id: String,
    pub label: String,
    pub connected: bool,
    pub can_disconnect: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SshKeyInfo {
    pub id: UserSshKeyId,
    pub name: String,
    pub fingerprint: String,
    pub created_at: NaiveDateTime,
    pub last_used_at: Option<NaiveDateTime>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct CreateSshKeyRequest {
    pub name: String,
    pub public_key: String,
}
