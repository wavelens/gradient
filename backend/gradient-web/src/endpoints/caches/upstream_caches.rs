/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::CacheRefusal;
use crate::access::{CacheAccess, Caller, load_cache, reject_managed_cache};
use crate::api_spec::CachePath;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::resource::Resource;
use crate::scope::{CacheScope, Readable, Requires, RequiresUnmanaged, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_core::upstream_source::{ProtocolProbe, probe_protocol};
use gradient_db::WebDb;
use gradient_entity::cache_upstream::{CacheUpstreamKind, CacheUpstreamSource};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::*;
use gradient_util::http::HttpVersion;
use sea_orm::ActiveValue::Set;
use sea_orm::IntoActiveModel;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, utoipa::ToSchema)]
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

#[derive(Serialize, utoipa::ToSchema)]
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

impl From<MCacheUpstream> for UpstreamCacheItem {
    fn from(upstream: MCacheUpstream) -> Self {
        Self {
            id: upstream.id,
            display_name: upstream.display_name,
            mode: upstream.mode,
            upstream_cache_id: upstream.upstream_cache,
            url: upstream.url,
            public_key: upstream.public_key,
            kind: format!("{:?}", upstream.kind).to_lowercase(),
            remote_cache: upstream.remote_cache_name,
            http1_only: upstream.http1_only,
            active: upstream.active,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
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
        return Err(CacheRefusal::UpstreamUrlRequired.into());
    }
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err(CacheRefusal::UpstreamUrlWithoutHttpScheme.into());
    }
    Ok(())
}

fn validate_http(url: &str, public_key: &str) -> Result<(), WebError> {
    validate_url(url)?;
    if public_key.trim().is_empty() {
        return Err(CacheRefusal::HttpPublicKeyRequired.into());
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
        return Err(CacheRefusal::ApiKeyOverCleartext.into());
    }
    let name = remote_cache.trim();
    if name.is_empty() {
        return Err(CacheRefusal::RemoteCacheNameRequired.into());
    }
    if name == "." || name == ".." {
        return Err(CacheRefusal::RemoteCacheNameInvalid.into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(CacheRefusal::RemoteCacheNameBadCharacters.into());
    }
    Ok(())
}

async fn load_upstream(
    db: &WebDb,
    cache_id: CacheId,
    upstream_id: CacheUpstreamId,
) -> WebResult<MCacheUpstream> {
    gradient_db::caches::upstream::in_cache(db, cache_id, upstream_id)
        .await?
        .or_not_found(Resource::UpstreamCache)
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/upstream-caches",
    operation_id = "listUpstreamCaches",
    tag = "caches",
    summary = "List upstream caches",
    description = "Returns all upstream caches configured for this cache (both internal Gradient caches and external binary caches), including each upstream's `public_key` so clients can trust pull-through paths. Follows the same visibility as `GET /caches/{cache}`: public caches are readable by anyone, private caches only by members and subscribed projects.",
    params(CachePath),
    responses((status = OK, description = "List of upstream entries", body = Vec<UpstreamCacheItem>), WebError)
)]
pub async fn get_upstream_caches(
    CacheScope { cache, db, .. }: CacheScope<Readable>,
) -> WebResult<Json<BaseResponse<Vec<UpstreamCacheItem>>>> {
    let upstream_caches = gradient_db::caches::upstream::of_cache(&db.web_db, cache.id)
        .await?
        .into_iter()
        .map(UpstreamCacheItem::from)
        .collect();

    Ok(ok_json(upstream_caches))
}

#[utoipa::path(
    put,
    path = "/caches/{cache}/upstream-caches",
    operation_id = "addUpstreamCache",
    tag = "caches",
    summary = "Add upstream cache",
    description = "Adds an upstream cache. Three types are supported, selected by the `type` discriminator:\n\n- **internal** - another Gradient-managed cache (referenced by name). Supports ReadOnly / ReadWrite / WriteOnly modes.\n- **gradient_proto** - a remote Gradient cache accessed via the binary WebSocket protocol (`/cache/{cache}/proto`). Supports optional `api_key` (stored encrypted at rest; write-only - never returned by the API) and pull mode.\n- **http** - an arbitrary Nix binary cache URL. Always ReadOnly.\n\nRequires `manageUpstreamCaches`.",
    params(CachePath),
    request_body = AddUpstreamRequest,
    responses((status = OK, description = "Upstream added - returns the new upstream UUID", body = CacheUpstreamId), WebError)
)]
pub async fn put_cache_upstream(
    CacheScope {
        cache,
        user,
        api_key,
        db,
        ..
    }: CacheScope<RequiresUnmanaged<can::ManageUpstreamCaches>>,
    state: State<Arc<ServerState>>,
    Json(body): Json<AddUpstreamRequest>,
) -> WebResult<Json<BaseResponse<CacheUpstreamId>>> {
    let record = match body {
        AddUpstreamRequest::Internal {
            cache_name,
            display_name,
            mode,
        } => {
            let upstream = load_cache(
                &db,
                Caller::User(&user),
                api_key.as_ref(),
                cache_name,
                CacheAccess::Readable,
            )
            .await?;
            if upstream.id == cache.id {
                return Err(CacheRefusal::CacheIsOwnUpstream.into());
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
                        .context("encrypt upstream API key")?,
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
        }
    };

    let inserted = gradient_db::rows::insert::<ECacheUpstream, _>(&state.web_db, record).await?;
    Ok(ok_json(inserted.id))
}

#[utoipa::path(
    patch,
    path = "/caches/{cache}/upstream-caches/{id}",
    operation_id = "patchUpstreamCache",
    tag = "caches",
    summary = "Update upstream cache",
    description = "Updates an upstream entry. For external upstream caches the mode is always forced to ReadOnly;\n`url` and `public_key` may be updated. For internal upstream caches only `mode` and `display_name` apply.\n`active` toggles any upstream cache. An inactive upstream cache is kept but never queried.\nA state-managed cache accepts only `active`, and the next server start restores the declared value.\nRequires `manageUpstreamCaches`.",
    params(CachePath, ("id" = CacheUpstreamId, Path)),
    request_body = PatchUpstreamRequest,
    responses((status = OK, description = "Updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_cache_upstream(
    CacheScope {
        cache, db, params, ..
    }: CacheScope<Requires<can::ManageUpstreamCaches>>,
    Json(body): Json<PatchUpstreamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let upstream_id: CacheUpstreamId = params.param("id")?;

    if patch_edits_managed_fields(&body) {
        reject_managed_cache(&cache)?;
    }
    let record = load_upstream(&db.web_db, cache.id, upstream_id).await?;

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

    gradient_db::rows::update::<ECacheUpstream, _>(&db.web_db, row).await?;

    Ok(ok_json("Upstream updated".to_string()))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
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

#[utoipa::path(
    post,
    path = "/caches/{cache}/upstream-caches/{id}/test",
    operation_id = "testUpstreamCache",
    tag = "caches",
    summary = "Test an HTTP upstream cache over HTTP/1.1 and HTTP/2",
    description = "Fetches the upstream cache's `nix-cache-info` twice in parallel, once over HTTP/1.1 only and once\nover HTTP/2 only, and reports each result. A probe passes on a 2xx body with a `StoreDir:` line.\nDoes not change the upstream cache's `http1_only` pin. Only `http` upstream caches can be tested.\nRequires `manageUpstreamCaches`; state-managed caches may be tested.",
    params(CachePath, ("id" = CacheUpstreamId, Path)),
    responses((status = OK, description = "Test result", body = UpstreamTestResponse), WebError)
)]
pub async fn post_cache_upstream_test(
    CacheScope {
        cache, db, params, ..
    }: CacheScope<Requires<can::ManageUpstreamCaches>>,
) -> WebResult<Json<BaseResponse<UpstreamTestResponse>>> {
    let upstream_id: CacheUpstreamId = params.param("id")?;

    let record = load_upstream(&db.web_db, cache.id, upstream_id).await?;
    let Some(CacheUpstreamSource::Http { url, .. }) = record.as_source() else {
        return Err(CacheRefusal::UpstreamNotHttp.into());
    };

    let (http1, http2) = tokio::join!(
        probe_protocol(url, HttpVersion::Http1),
        probe_protocol(url, HttpVersion::Http2),
    );

    Ok(ok_json(UpstreamTestResponse::new(http1, http2)))
}

#[utoipa::path(
    delete,
    path = "/caches/{cache}/upstream-caches/{id}",
    operation_id = "deleteUpstreamCache",
    tag = "caches",
    summary = "Remove upstream cache",
    params(CachePath, ("id" = CacheUpstreamId, Path)),
    responses((status = OK, description = "Removed", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_cache_upstream(
    CacheScope {
        cache, db, params, ..
    }: CacheScope<RequiresUnmanaged<can::ManageUpstreamCaches>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let upstream_id: CacheUpstreamId = params.param("id")?;

    let record = load_upstream(&db.web_db, cache.id, upstream_id).await?;

    gradient_db::rows::delete::<ECacheUpstream, _>(&db.web_db, record).await?;

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
