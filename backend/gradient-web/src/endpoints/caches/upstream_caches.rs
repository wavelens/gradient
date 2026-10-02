/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, load_cache, reject_managed_cache};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::CachePermission;
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use gradient_core::ServerState;
use gradient_core::upstream_source::{ProtocolProbe, probe_protocol};
use gradient_entity::cache_upstream::{CacheUpstreamKind, CacheUpstreamSource};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::*;
use gradient_util::http::HttpVersion;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AddUpstreamRequest {
    Internal {
        cache_name: String,
        display_name: Option<String>,
        mode: Option<CacheSubscriptionMode>,
    },
    Http {
        display_name: String,
        url: String,
        public_key: String,
    },
    GradientProto {
        url: String,
        remote_cache: String,
        display_name: String,
        mode: Option<CacheSubscriptionMode>,
        api_key: Option<String>,
    },
}

#[derive(Serialize)]
pub struct UpstreamCacheItem {
    pub id: CacheUpstreamId,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    pub upstream_cache_id: Option<CacheId>,
    pub url: Option<String>,
    pub public_key: Option<String>,
    pub kind: String,
    pub remote_cache: Option<String>,
    pub http1_only: bool,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct PatchUpstreamRequest {
    pub display_name: Option<String>,
    pub mode: Option<CacheSubscriptionMode>,
    pub url: Option<String>,
    pub public_key: Option<String>,
    pub active: Option<bool>,
}

/// State is re-applying a managed cache's upstream caches on startup. Toggling `active` is the only
/// edit worth allowing there.
fn patch_edits_managed_fields(body: &PatchUpstreamRequest) -> bool {
    body.display_name.is_some()
        || body.mode.is_some()
        || body.url.is_some()
        || body.public_key.is_some()
}

fn validate_url(url: &str) -> Result<(), WebError> {
    let u = url.trim();
    if u.is_empty() {
        return Err(WebError::bad_request("Substituter URL is required."));
    }
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err(WebError::bad_request(
            "Substituter URL must start with http:// or https://.",
        ));
    }
    Ok(())
}

fn validate_http(url: &str, public_key: &str) -> Result<(), WebError> {
    validate_url(url)?;
    if public_key.trim().is_empty() {
        return Err(WebError::bad_request(
            "Public key is required for an Http binary cache.",
        ));
    }
    Ok(())
}

fn validate_gradient_proto(
    url: &str,
    remote_cache: &str,
    api_key: Option<&str>,
) -> Result<(), WebError> {
    validate_url(url)?;
    if api_key.is_some_and(|k| !k.trim().is_empty()) && !url.trim().starts_with("https://") {
        return Err(WebError::bad_request(
            "An API key requires an https:// upstream URL so the key is not transmitted in cleartext.",
        ));
    }
    let name = remote_cache.trim();
    if name.is_empty() {
        return Err(WebError::bad_request(
            "Remote cache name is required for a Gradient Proto upstream.",
        ));
    }
    if name == "." || name == ".." {
        return Err(WebError::bad_request("Remote cache name is invalid."));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(WebError::bad_request(
            "Remote cache name may only contain letters, digits, '-', '_', and '.'.",
        ));
    }
    Ok(())
}

async fn load_upstream(
    state: &Arc<ServerState>,
    cache_id: CacheId,
    upstream_id: CacheUpstreamId,
) -> WebResult<MCacheUpstream> {
    ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Upstream cache")
}

pub async fn get_upstream_caches(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<UpstreamCacheItem>>>> {
    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

    let upstream_caches = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|u| UpstreamCacheItem {
            id: u.id,
            display_name: u.display_name,
            mode: u.mode,
            upstream_cache_id: u.upstream_cache,
            url: u.url,
            public_key: u.public_key,
            kind: format!("{:?}", u.kind).to_lowercase(),
            remote_cache: u.remote_cache_name,
            http1_only: u.http1_only,
            active: u.active,
        })
        .collect();

    Ok(ok_json(upstream_caches))
}

pub async fn put_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<AddUpstreamRequest>,
) -> WebResult<Json<BaseResponse<CacheUpstreamId>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageUpstreamCaches,
            reject_managed: true,
        },
    )
    .await?;

    let record = match body {
        AddUpstreamRequest::Internal {
            cache_name,
            display_name,
            mode,
        } => {
            let upstream = load_cache(
                &state,
                Caller::User(&user),
                api_key.as_ref(),
                cache_name,
                CacheAccess::Readable,
            )
            .await?;
            if upstream.id == cache.id {
                return Err(WebError::bad_request("A cache cannot be its own upstream"));
            }
            let name = display_name.unwrap_or_else(|| upstream.display_name.clone());
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: cache.id,
                display_name: name,
                mode: mode.unwrap_or(CacheSubscriptionMode::ReadWrite),
                kind: CacheUpstreamKind::Internal,
                upstream_cache: Some(upstream.id),
                ..Default::default()
            }
            .into_active_model()
        }
        AddUpstreamRequest::Http {
            display_name,
            url,
            public_key,
        } => {
            validate_http(&url, &public_key)?;
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: cache.id,
                display_name,
                mode: CacheSubscriptionMode::ReadOnly,
                kind: CacheUpstreamKind::Http,
                url: Some(url.trim().to_string()),
                public_key: Some(public_key),
                ..Default::default()
            }
            .into_active_model()
        }
        AddUpstreamRequest::GradientProto {
            url,
            remote_cache,
            display_name,
            mode,
            api_key: key,
        } => {
            validate_gradient_proto(&url, &remote_cache, key.as_deref())?;
            let api_key_enc = match key {
                Some(k) if !k.trim().is_empty() => Some(
                    gradient_sources::encrypt_secret(&state.config.secrets.crypt_file, k.trim())
                        .map_err(|_| WebError::internal("Failed to encrypt upstream API key"))?,
                ),
                _ => None,
            };
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: cache.id,
                display_name,
                mode: mode.unwrap_or(CacheSubscriptionMode::ReadOnly),
                kind: CacheUpstreamKind::GradientProto,
                url: Some(url.trim().to_string()),
                remote_cache_name: Some(remote_cache.trim().to_string()),
                api_key: api_key_enc,
                ..Default::default()
            }
            .into_active_model()
        }
    };

    let inserted = record.insert(&state.web_db).await?;
    Ok(ok_json(inserted.id))
}

pub async fn patch_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, upstream_id)): Path<(String, CacheUpstreamId)>,
    Json(body): Json<PatchUpstreamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageUpstreamCaches,
            reject_managed: false,
        },
    )
    .await?;
    if patch_edits_managed_fields(&body) {
        reject_managed_cache(&cache)?;
    }
    let record = load_upstream(&state, cache.id, upstream_id).await?;

    let is_external = matches!(record.as_source(), Some(CacheUpstreamSource::Http { .. }));
    let mut row = record.into_active_model();

    if let Some(name) = body.display_name {
        row.display_name = Set(name);
    }
    if let Some(enabled) = body.active {
        row.active = Set(enabled);
    }
    if is_external {
        row.mode = Set(CacheSubscriptionMode::ReadOnly);
        if let Some(url) = body.url {
            row.url = Set(Some(url));
        }
        if let Some(key) = body.public_key {
            row.public_key = Set(Some(key));
        }
    } else if let Some(mode) = body.mode {
        row.mode = Set(mode);
    }

    row.update(&state.web_db).await?;

    Ok(ok_json("Upstream updated".to_string()))
}

#[derive(Debug, Serialize)]
pub struct UpstreamTestResponse {
    pub ok: bool,
    pub http1: ProtocolProbe,
    pub http2: ProtocolProbe,
    pub message: String,
}

impl UpstreamTestResponse {
    fn new(http1: ProtocolProbe, http2: ProtocolProbe) -> Self {
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

pub async fn post_cache_upstream_test(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, upstream_id)): Path<(String, CacheUpstreamId)>,
) -> WebResult<Json<BaseResponse<UpstreamTestResponse>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageUpstreamCaches,
            reject_managed: false,
        },
    )
    .await?;
    let record = load_upstream(&state, cache.id, upstream_id).await?;
    let Some(CacheUpstreamSource::Http { url, .. }) = record.as_source() else {
        return Err(WebError::bad_request(
            "Only HTTP binary-cache upstreams can be tested.",
        ));
    };

    let (http1, http2) = tokio::join!(
        probe_protocol(url, HttpVersion::Http1),
        probe_protocol(url, HttpVersion::Http2),
    );

    Ok(ok_json(UpstreamTestResponse::new(http1, http2)))
}

pub async fn delete_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, upstream_id)): Path<(String, CacheUpstreamId)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageUpstreamCaches,
            reject_managed: true,
        },
    )
    .await?;
    let record = load_upstream(&state, cache.id, upstream_id).await?;

    let active: ACacheUpstream = record.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json("Upstream removed".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_is_editable_on_a_managed_cache() {
        let patch = |json| serde_json::from_value::<PatchUpstreamRequest>(json).unwrap();
        assert!(!patch_edits_managed_fields(&patch(
            serde_json::json!({ "active": false })
        )));
        for edit in [
            serde_json::json!({ "display_name": "x" }),
            serde_json::json!({ "mode": "ReadOnly" }),
            serde_json::json!({ "url": "https://x" }),
            serde_json::json!({ "public_key": "k:1" }),
        ] {
            assert!(patch_edits_managed_fields(&patch(edit)));
        }
    }

    #[test]
    fn validate_http_requires_url_and_key() {
        assert!(validate_http("", "k").is_err());
        assert!(validate_http("https://x", "").is_err());
        assert!(validate_http("not a url", "k").is_err());
        assert!(validate_http("https://cache.nixos.org", "cache.nixos.org-1:abc").is_ok());
    }

    #[test]
    fn validate_gradient_proto_requires_url_and_remote_cache() {
        assert!(validate_gradient_proto("", "prod", None).is_err());
        assert!(validate_gradient_proto("https://x", "", None).is_err());
        assert!(validate_gradient_proto("ftp://x", "prod", None).is_err());
        assert!(validate_gradient_proto("https://remote.example", "prod", None).is_ok());
    }

    #[test]
    fn validate_gradient_proto_requires_https_when_api_key_present() {
        assert!(validate_gradient_proto("http://remote.example", "prod", Some("secret")).is_err());
        assert!(validate_gradient_proto("https://remote.example", "prod", Some("secret")).is_ok());
        assert!(validate_gradient_proto("http://remote.example", "prod", None).is_ok());
        assert!(validate_gradient_proto("http://remote.example", "prod", Some("   ")).is_ok());
    }

    #[test]
    fn validate_gradient_proto_rejects_unsafe_remote_cache() {
        assert!(validate_gradient_proto("https://x", "a/b", None).is_err());
        assert!(validate_gradient_proto("https://x", "..", None).is_err());
        assert!(validate_gradient_proto("https://x", "x?y=1", None).is_err());
        assert!(validate_gradient_proto("https://x", "has space", None).is_err());
        assert!(validate_gradient_proto("https://x", "prod-1.cache_2", None).is_ok());
    }
}
