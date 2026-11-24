/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerOptionValueSource {
    Flag,
    Env,
    Default,
    Unset,
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub struct EffectiveConfigOption {
    pub group: String,
    pub field: String,
    pub flag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub secret: bool,
    pub source: ServerOptionValueSource,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ChunkReceived {
    pub received: u64,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct LoginProviderInfo {
    pub id: String,
    pub label: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct GiteaOAuthAppConfig {
    pub git_host_type: String,
    pub instance_base_url: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ServerConfig {
    pub version: String,
    pub commit: Option<String>,
    pub oidc_enabled: bool,
    pub login_providers: Vec<LoginProviderInfo>,
    pub password_login_enabled: bool,
    pub registration_enabled: bool,
    pub email_verification_enabled: bool,
    pub smtp_enabled: bool,
    pub quic: bool,
    pub create_project: CreatePermission,
    pub create_team: CreatePermission,
    pub create_cache: CreatePermission,
    pub github_app_enabled: bool,
    pub github_app_page_url: Option<String>,
    pub github_app_install_url: Option<String>,
    pub gitea_oauth_app: Option<GiteaOAuthAppConfig>,
    pub ssh_enabled: bool,
    pub ssh_port: Option<u16>,
    pub gradient_ci_enabled: bool,
    pub gradient_ci_url: String,
    pub serve_url: String,
    pub logo_url: Option<String>,
    pub exec_enabled: bool,
    pub public_listing: bool,
    pub max_keep_evaluations_per_task: Option<i32>,
    /// Limit for cached NAR storage across the instance, in GB (`cache.maxStorageGb`). This field does not exist when the limit is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cached_nar_storage_gb: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, utoipa::ToSchema)]
pub struct ErrorResponseBody {
    pub error: bool,
    pub code: String,
    pub message: String,
    /// Wire names of the permissions that the signed-in caller does not have for this request (for example, `readStore`). Only 401 and 403 responses to authenticated callers include this field. The names match the values of `permissions` on the API key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_permissions: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreatedEntityResponse {
    pub id: String,
    pub name: String,
    pub url: String,
}

impl CreatedEntityResponse {
    pub fn new(id: impl ToString, name: impl Into<String>, url: String) -> Self {
        Self {
            id: id.to_string(),
            name: name.into(),
            url,
        }
    }
}
