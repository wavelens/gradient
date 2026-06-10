/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, load_cache};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::CachePermission;
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use gradient_entity::cache_upstream::{CacheUpstreamKind, CacheUpstreamSource};
use gradient_entity::organization_cache::CacheSubscriptionMode;
use gradient_types::*;
use gradient_core::ServerState;
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
}

#[derive(Debug, Deserialize)]
pub struct PatchUpstreamRequest {
    pub display_name: Option<String>,
    pub mode: Option<CacheSubscriptionMode>,
    pub url: Option<String>,
    pub public_key: Option<String>,
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

pub async fn get_cache_upstreams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<UpstreamCacheItem>>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ViewCache,
            reject_managed: false,
        },
    )
    .await?;

    let upstreams = ECacheUpstream::find()
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
        })
        .collect();

    Ok(ok_json(upstreams))
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
            permission: CachePermission::ManageCacheUpstreams,
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
                return Err(WebError::bad_request(
                    "A cache cannot be its own upstream",
                ));
            }
            let name = display_name.unwrap_or_else(|| upstream.display_name.clone());
            ACacheUpstream {
                id: Set(CacheUpstreamId::now_v7()),
                cache: Set(cache.id),
                display_name: Set(name),
                mode: Set(mode.unwrap_or(CacheSubscriptionMode::ReadWrite)),
                kind: Set(CacheUpstreamKind::Internal),
                upstream_cache: Set(Some(upstream.id)),
                url: Set(None),
                public_key: Set(None),
                remote_cache_name: Set(None),
                api_key: Set(None),
            }
        }
        AddUpstreamRequest::Http {
            display_name,
            url,
            public_key,
        } => {
            validate_http(&url, &public_key)?;
            ACacheUpstream {
                id: Set(CacheUpstreamId::now_v7()),
                cache: Set(cache.id),
                display_name: Set(display_name),
                mode: Set(CacheSubscriptionMode::ReadOnly),
                kind: Set(CacheUpstreamKind::Http),
                upstream_cache: Set(None),
                url: Set(Some(url.trim().to_string())),
                public_key: Set(Some(public_key)),
                remote_cache_name: Set(None),
                api_key: Set(None),
            }
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
                    gradient_core::sources::encrypt_secret(
                        &state.config.secrets.crypt_secret_file,
                        k.trim(),
                    )
                    .map_err(|_| WebError::internal("Failed to encrypt upstream API key"))?,
                ),
                _ => None,
            };
            ACacheUpstream {
                id: Set(CacheUpstreamId::now_v7()),
                cache: Set(cache.id),
                display_name: Set(display_name),
                mode: Set(mode.unwrap_or(CacheSubscriptionMode::ReadOnly)),
                kind: Set(CacheUpstreamKind::GradientProto),
                upstream_cache: Set(None),
                url: Set(Some(url.trim().to_string())),
                public_key: Set(None),
                remote_cache_name: Set(Some(remote_cache.trim().to_string())),
                api_key: Set(api_key_enc),
            }
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
            permission: CachePermission::ManageCacheUpstreams,
            reject_managed: true,
        },
    )
    .await?;
    let record = load_upstream(&state, cache.id, upstream_id).await?;

    let is_external = matches!(record.as_source(), Some(CacheUpstreamSource::Http { .. }));
    let mut active = record.into_active_model();

    if let Some(name) = body.display_name {
        active.display_name = Set(name);
    }
    if is_external {
        active.mode = Set(CacheSubscriptionMode::ReadOnly);
        if let Some(url) = body.url {
            active.url = Set(Some(url));
        }
        if let Some(key) = body.public_key {
            active.public_key = Set(Some(key));
        }
    } else if let Some(mode) = body.mode {
        active.mode = Set(mode);
    }

    active.update(&state.web_db).await?;

    Ok(ok_json("Upstream updated".to_string()))
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
            permission: CachePermission::ManageCacheUpstreams,
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
        // http is fine when no key is transmitted.
        assert!(validate_gradient_proto("http://remote.example", "prod", None).is_ok());
        // A blank key is treated as no key.
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
