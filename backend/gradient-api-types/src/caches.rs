/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use chrono::NaiveDateTime;
use utoipa::ToSchema;

use serde::Serialize;

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(sea_orm::FromQueryResult)
)]
pub struct NarSummary {
    pub hash: String,
    pub store_path: String,
    pub package: String,
    pub nar_size: Option<i64>,
    pub file_size: Option<i64>,
    pub created_at: NaiveDateTime,
    pub last_fetched_at: Option<NaiveDateTime>,
    pub pinned_at: Option<NaiveDateTime>,
    pub pinned_by_username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct ProtocolProbe {
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeCacheRequest {
    pub name: CacheName,
    pub display_name: CacheDisplayName,
    pub description: String,
    pub priority: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_storage_gb: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_through: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_team: Option<String>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct CacheResponse {
    pub id: CacheId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub local_priority: Option<i32>,
    pub max_storage_gb: i32,
    pub pull_through: bool,
    pub public_key: String,
    pub public: bool,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    pub owner_team: Option<TeamNameAndDisplayName>,
    pub can_edit: bool,
    pub permissions: Vec<CachePermission>,
    pub role: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchCacheRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<CacheName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_storage_gb: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_through: Option<bool>,
    /// Omitted fields leave the owner team unchanged. A team name can set the owner team. JSON null can clear the owner team.
    #[serde(
        default,
        deserialize_with = "crate::deserialize_optional_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_team: Option<Option<String>>,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct CachedPathPinStateChangeCount {
    pub pin_state_changed_path_count: u64,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct NarDetail {
    pub hash: String,
    pub store_path: String,
    pub package: String,
    pub nar_size: Option<i64>,
    pub file_size: Option<i64>,
    pub file_hash: Option<String>,
    pub nar_hash: Option<String>,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub created_at: NaiveDateTime,
    pub last_fetched_at: Option<NaiveDateTime>,
    pub fetch_count: i64,
    pub signed: bool,
    pub pinned_at: Option<NaiveDateTime>,
    pub pinned_by_username: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct NarStats {
    pub total_nars: i64,
    pub total_nar_size: i64,
    pub total_file_size: i64,
    pub last_uploaded_at: Option<NaiveDateTime>,
    pub oldest_fetched_at: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub struct SubscriptionRequestItem {
    pub cache: String,
    pub cache_display_name: String,
    pub project: String,
    pub project_display_name: String,
    pub mode: CacheSubscriptionMode,
    pub requested_by: String,
    pub requested_by_display_name: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub struct CacheSubscriberItem {
    pub project: String,
    pub project_display_name: String,
    pub mode: CacheSubscriptionMode,
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub struct CacheSubscribersResponse {
    pub projects: Vec<CacheSubscriberItem>,
    pub hidden: usize,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct NarinfoPart {
    pub store_path: String,
    pub file_hash: String,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: String,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub deriver: Option<String>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AddUpstreamRequest {
    Internal {
        cache_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<CacheSubscriptionMode>,
    },
    Http {
        display_name: String,
        url: String,
        public_key: String,
        #[serde(default)]
        http1_only: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        credential: Option<HttpUpstreamCredentialRequest>,
    },
    GradientProto {
        url: String,
        remote_cache: String,
        display_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<CacheSubscriptionMode>,
        #[serde(skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
    },
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum HttpUpstreamCredentialRequest {
    Basic { user: String, password: String },
    Bearer { token: String },
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum PatchHttpUpstreamCredentialRequest {
    Basic { user: String, password: String },
    Bearer { token: String },
    None,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSchemeItem {
    Basic,
    Bearer,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct UpstreamCacheItem {
    pub id: CacheUpstreamId,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    pub upstream_cache_id: Option<CacheId>,
    /// The name of the internal upstream cache (for a link to that cache). The API can return this name when the cache is public or the caller has `manageUpstreamCaches` here.
    pub upstream_cache_name: Option<String>,
    pub url: Option<String>,
    pub public_key: Option<String>,
    pub kind: String,
    pub remote_cache: Option<String>,
    pub http1_only: bool,
    pub active: bool,
    /// True when the stored URL holds a user, password or query, which `url` leaves out. Such an upstream cache can't be set active until its URL is fixed: with PATCH for an HTTP upstream cache, by creating it again for a Gradient Proto one.
    pub stored_url_has_user_password_or_query: bool,
    pub credential_scheme: Option<CredentialSchemeItem>,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PatchUpstreamRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<CacheSubscriptionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http1_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<PatchHttpUpstreamCredentialRequest>,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct UpstreamTestResponse {
    pub ok: bool,
    pub http1: ProtocolProbe,
    pub http2: ProtocolProbe,
    pub message: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct CacheMetricPoint {
    pub time: String,
    pub bytes: i64,
    pub requests: i64,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct StorageMetricPoint {
    pub time: String,
    pub packages: i64,
    pub bytes: i64,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct CacheStatsResponse {
    pub total_bytes: i64,
    pub total_nar_bytes: i64,
    pub total_packages: i64,
    pub storage_minutes: Vec<StorageMetricPoint>,
    pub storage_hours: Vec<StorageMetricPoint>,
    pub storage_days: Vec<StorageMetricPoint>,
    pub storage_weeks: Vec<StorageMetricPoint>,
    pub minutes: Vec<CacheMetricPoint>,
    pub hours: Vec<CacheMetricPoint>,
    pub days: Vec<CacheMetricPoint>,
    pub weeks: Vec<CacheMetricPoint>,
}

impl std::fmt::Debug for PatchHttpUpstreamCredentialRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Basic { .. } => "Basic",
            Self::Bearer { .. } => "Bearer",
            Self::None => "None",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, ToSchema)]
pub struct Cache {
    pub id: CacheId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub local_priority: Option<i32>,
    pub public_key: String,
    pub public: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
    pub max_storage_gb: i32,
    pub pull_through: bool,
    pub owner_team: Option<TeamId>,
}

impl PatchHttpUpstreamCredentialRequest {
    pub fn into_credential_request_to_store(self) -> Option<HttpUpstreamCredentialRequest> {
        match self {
            Self::Basic { user, password } => {
                Some(HttpUpstreamCredentialRequest::Basic { user, password })
            }
            Self::Bearer { token } => Some(HttpUpstreamCredentialRequest::Bearer { token }),
            Self::None => None,
        }
    }
}

impl UpstreamTestResponse {
    pub fn new(http1: ProtocolProbe, http2: ProtocolProbe) -> Self {
        let message = match (http1.ok, http2.ok) {
            (true, true) => "Reachable over HTTP/1.1 and HTTP/2.",
            (true, false) => "Reachable over HTTP/1.1 only.",
            (false, true) => "Reachable over HTTP/2 only.",
            (false, false) => "Unreachable over HTTP/1.1 and HTTP/2.",
        };
        Self {
            ok: http1.ok || http2.ok,
            http1,
            http2,
            message: message.to_string(),
        }
    }
}
