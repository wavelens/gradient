/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::decode_jwt;
use crate::client_ip::resolve_client_ip;
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::OptionExt;
use crate::ip_allowlist::is_allowed as ip_allowed;
use axum::extract::State;
use axum::http::HeaderMap;
use base64::Engine;
use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gradient_core::ServerState;
use gradient_graph::Demotion;
use gradient_sources::get_path_from_derivation_output;
use gradient_storage::NarSource;
use gradient_types::*;
use gradient_util::nix_hash::{normalize_nar_hash, strip_hash_algo};
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, EntityTrait, QueryFilter};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

async fn try_authenticate_basic(
    state: &Arc<ServerState>,
    headers: &HeaderMap,
    client_ip: IpAddr,
) -> WebResult<Option<MUser>> {
    let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Ok(None);
    };
    let Ok(val) = auth.to_str() else {
        return Ok(None);
    };
    let Some(encoded) = val.strip_prefix("Basic ") else {
        return Ok(None);
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return Ok(None);
    };
    let Ok(creds) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    let Some(password) = creds.split_once(':').map(|(_, p)| p.to_string()) else {
        return Ok(None);
    };
    let Ok(decoded) = decode_jwt(State(Arc::clone(state)), password).await else {
        return Ok(None);
    };
    if let Some(ctx) = decoded.api_key_context()
        && !ip_allowed(client_ip, &ctx.allowed_ips)
    {
        return Err(WebError::forbidden_with(
            ErrorCode::FORBIDDEN_SOURCE_IP,
            "API key not allowed from this source IP",
        ));
    }
    Ok(EUser::find_by_id(decoded.user_id())
        .one(&state.web_db)
        .await
        .ok()
        .flatten())
}

async fn user_can_access_cache(state: &Arc<ServerState>, cache: &MCache, user: &MUser) -> bool {
    if cache.created_by == user.id {
        return true;
    }

    let direct = ECacheAccess::find()
        .filter(CCacheAccess::Cache.eq(cache.id))
        .filter(CCacheAccess::User.eq(user.id))
        .one(&state.web_db)
        .await
        .unwrap_or(None);
    if direct.is_some() {
        return true;
    }

    let project_ids: Vec<ProjectId> = EProjectAccess::find()
        .filter(CProjectAccess::User.eq(user.id))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|ou| ou.project)
        .collect();

    if project_ids.is_empty() {
        return false;
    }

    EProjectCache::find()
        .filter(CProjectCache::Cache.eq(cache.id))
        .filter(CProjectCache::Project.is_in(project_ids))
        .one(&state.web_db)
        .await
        .unwrap_or(None)
        .is_some()
}

async fn require_cache_auth(
    state: &Arc<ServerState>,
    headers: &HeaderMap,
    client_ip: IpAddr,
    cache: &MCache,
) -> WebResult<()> {
    if cache.public {
        return Ok(());
    }

    let maybe_user = try_authenticate_basic(state, headers, client_ip).await?;
    match maybe_user {
        Some(user) if user_can_access_cache(state, cache, &user).await => Ok(()),
        _ => Err(WebError::unauthorized(
            "Authentication required to access this cache".to_string(),
        )),
    }
}

pub(super) async fn get_nar_by_hash(
    state: Arc<ServerState>,
    cache: MCache,
    hash: String,
) -> Result<NixPathInfo, WebError> {
    get_nar_by_hash_inner(&state, cache, hash).await
}

async fn get_nar_by_hash_inner(
    state: &Arc<ServerState>,
    cache: MCache,
    hash: String,
) -> Result<NixPathInfo, WebError> {
    let build_output = EDerivationOutput::find()
        .filter(
            Condition::all()
                .add(CDerivationOutput::IsCached.eq(true))
                .add(CDerivationOutput::Hash.eq(hash.clone())),
        )
        .one(&state.web_db)
        .await
        .map_err(WebError::from)?;

    let build_output = match build_output {
        Some(o) => o,
        None => return get_nar_by_cached_path(state, cache, hash).await,
    };

    let cached_path_row = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash.clone()))
        .one(&state.web_db)
        .await
        .map_err(WebError::from)?
        .or_not_found("CachedPath")?;

    let cached_path_sig = ECachedPathSignature::find()
        .filter(
            Condition::all()
                .add(CCachedPathSignature::CachedPath.eq(cached_path_row.id))
                .add(CCachedPathSignature::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await
        .map_err(WebError::from)?
        .or_not_found("Signature")?;

    let signature = cached_path_sig
        .signature
        .or_not_found("Signature not yet computed")?;

    let path = get_path_from_derivation_output(build_output.clone()).full();

    let nar_hash = cached_path_row
        .nar_hash
        .as_deref()
        .map(normalize_nar_hash)
        .or_not_found("NarHash not recorded")?;
    let nar_size = cached_path_row
        .nar_size
        .or_not_found("NarSize not recorded")? as u64;
    let references = gradient_db::graph::runtime_closure::references_for_hash(
        &state.web_db,
        &cached_path_row.hash,
    )
    .await?;
    let deriver = cached_path_row.deriver.clone();
    let ca = cached_path_row.ca.clone();

    let sig = gradient_sources::full_signature_token(
        &signature,
        &state.config.server.serve_url,
        &cache.name,
    );

    // `file_hash` and `file_size` are living on `cached_path`. The legacy mirror on
    // `derivation_output` is not always populated.
    let file_hash = cached_path_row
        .file_hash
        .as_deref()
        .map(normalize_nar_hash)
        .or_not_found("FileHash not recorded")?;
    let file_hash_nix32 = strip_hash_algo(&file_hash).to_string();
    let file_size = cached_path_row
        .file_size
        .or_not_found("FileSize not recorded")? as u32;

    Ok(NixPathInfo {
        store_path: path,
        url: format!("nar/{}.nar.zst", file_hash_nix32),
        compression: "zstd".to_string(),
        file_hash,
        file_size,
        nar_hash,
        nar_size,
        references,
        deriver,
        sig,
        ca,
    })
}

async fn get_nar_by_cached_path(
    state: &Arc<ServerState>,
    cache: MCache,
    hash: String,
) -> Result<NixPathInfo, WebError> {
    let cached_path_row = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash.clone()))
        .one(&state.web_db)
        .await
        .map_err(WebError::from)?
        .or_not_found("Path")?;

    if !cached_path_row.is_fully_cached() {
        return Err(WebError::not_found("Path"));
    }

    let cached_path_sig = ECachedPathSignature::find()
        .filter(
            Condition::all()
                .add(CCachedPathSignature::CachedPath.eq(cached_path_row.id))
                .add(CCachedPathSignature::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await
        .map_err(WebError::from)?
        .or_not_found("Signature")?;

    let signature = cached_path_sig
        .signature
        .or_not_found("Signature not yet computed")?;

    let sig = gradient_sources::full_signature_token(
        &signature,
        &state.config.server.serve_url,
        &cache.name,
    );

    let file_hash = cached_path_row
        .file_hash
        .clone()
        .ok_or_else(|| WebError::bad_request("Missing file hash"))?;
    let file_size = cached_path_row
        .file_size
        .ok_or_else(|| WebError::bad_request("Missing file size"))? as u32;
    let nar_hash = cached_path_row
        .nar_hash
        .as_deref()
        .map(normalize_nar_hash)
        .or_not_found("NarHash not recorded")?;
    let nar_size = cached_path_row
        .nar_size
        .or_not_found("NarSize not recorded")? as u64;
    let references = gradient_db::graph::runtime_closure::references_for_hash(
        &state.web_db,
        &cached_path_row.hash,
    )
    .await?;
    let file_hash_nix32 = strip_hash_algo(&normalize_nar_hash(&file_hash)).to_string();

    Ok(NixPathInfo {
        store_path: cached_path_row.store_path(),
        url: format!("nar/{}.nar.zst", file_hash_nix32),
        compression: "zstd".to_string(),
        file_hash,
        file_size,
        nar_hash,
        nar_size,
        references,
        deriver: cached_path_row.deriver.clone(),
        sig,
        ca: cached_path_row.ca.clone(),
    })
}

pub(super) struct CacheContext {
    pub cache: MCache,
}

impl CacheContext {
    pub(super) async fn load(
        state: &Arc<ServerState>,
        headers: &HeaderMap,
        client_ip: IpAddr,
        cache_name: String,
    ) -> WebResult<Self> {
        let cache = ECache::find()
            .filter(CCache::Name.eq(cache_name))
            .one(&state.web_db)
            .await?
            .or_not_found("Cache")?;

        if !cache.active {
            return Err(WebError::bad_request("Cache is disabled"));
        }

        require_cache_auth(state, headers, client_ip, &cache).await?;

        Ok(Self { cache })
    }
}

pub(super) fn cache_client_ip(
    state: &Arc<ServerState>,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> IpAddr {
    let peer_ip = peer
        .map(|p| p.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    resolve_client_ip(headers, peer_ip, &state.config.network.trusted_proxies)
}

#[derive(Debug, serde::Deserialize)]
pub struct JsonFlag {
    pub json: Option<String>,
}

impl JsonFlag {
    pub fn is_set(&self) -> bool {
        self.json.is_some()
    }
}

gradient_db::sql! {
    CACHE_SERVES_PATH = "SELECT 1 AS served \
         FROM cached_path cp \
         JOIN cached_path_signature s ON s.cached_path = cp.id \
         WHERE cp.hash = $1 AND s.cache = $2 \
           AND s.signature IS NOT NULL AND cp.file_hash IS NOT NULL",
        params = [CachedPathHash, CacheId];

    CACHE_SERVED_DERIVATION = "SELECT d.id AS id \
         FROM derivation d \
         JOIN derivation_output o ON o.derivation = d.id \
         JOIN cached_path cp ON cp.hash = o.hash \
         JOIN cached_path_signature s ON s.cached_path = cp.id \
         WHERE d.hash = $1 AND d.name = $2 AND s.cache = $3 \
           AND s.signature IS NOT NULL AND cp.file_hash IS NOT NULL \
         LIMIT 1",
        params = [DerivationHash, Text("hello"), CacheId];
}

/// Blobs are living in one store for every cache. This signed claim is all that is keeping one
/// cache's paths out of another's.
pub(super) async fn cache_serves_path(
    state: &Arc<ServerState>,
    cache: CacheId,
    store_hash: &str,
) -> WebResult<bool> {
    Ok(state
        .web_db
        .query_one_raw(CACHE_SERVES_PATH.bind([store_hash.into(), cache.into_inner().into()]))
        .await?
        .is_some())
}

pub(super) async fn cache_served_derivation(
    state: &Arc<ServerState>,
    cache: CacheId,
    drv_hash: &str,
    drv_name: &str,
) -> WebResult<Option<DerivationId>> {
    let row = state
        .web_db
        .query_one_raw(CACHE_SERVED_DERIVATION.bind([
            drv_hash.into(),
            drv_name.into(),
            cache.into_inner().into(),
        ]))
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new))
}

pub async fn fetch_nar_stream(
    state: &Arc<ServerState>,
    cache: CacheId,
    path_hash: &str,
) -> WebResult<(String, u64, BoxStream<'static, anyhow::Result<Bytes>>)> {
    let effective_hash =
        crate::endpoints::caches::nar::resolve_effective_hash_db(&state.web_db, path_hash).await?;
    if !cache_serves_path(state, cache, &effective_hash).await? {
        return Err(WebError::not_found("Path"));
    }
    let source = state
        .nar_storage
        .open(&effective_hash, 0)
        .await
        .map_err(|e| WebError::internal(format!("Failed to read NAR: {}", e)))?
        .or_not_found("Path")?;
    let (size, stream) = match source {
        NarSource::Hot(bytes) => {
            let size = bytes.len() as u64;
            (
                size,
                futures::stream::once(async move { Ok(bytes) }).boxed(),
            )
        }
        NarSource::Stream { size, stream } => (size, stream),
    };
    Ok((effective_hash, size, stream))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DeleteOutcome {
    pub ref_counted_others: bool,
}

pub(super) async fn delete_nar_from_cache(
    state: &Arc<ServerState>,
    cache_id: CacheId,
    hash: &str,
) -> WebResult<(MCachedPath, DeleteOutcome)> {
    let report = state
        .graph
        .demote(Demotion::CacheClaim {
            cache: cache_id,
            hash: hash.to_owned(),
        })
        .await
        .map_err(|e| WebError::internal(e.to_string()))?;
    let cached_path = report.cached_path.or_not_found("Nar")?;
    state.nar_storage.hot().invalidate(hash);

    Ok((
        cached_path,
        DeleteOutcome {
            ref_counted_others: report.others_remain,
        },
    ))
}
