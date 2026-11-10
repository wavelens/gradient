/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::CacheRefusal;
use crate::access::effective_cache_mask;
use crate::authorization::{ApiKeyContext, DecodedRequest, decode_jwt};
use crate::client_ip::resolve_client_ip;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::ip_allowlist::is_allowed as ip_allowed;
use crate::permissions::{CachePermission, cache_mask_grants};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::store_paths::original_text;
use anyhow::Context;
use axum::http::HeaderMap;
use base64::Engine;
use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_graph::Demotion;
use gradient_graph::Graph;
use gradient_storage::NarSource;
use gradient_storage::NarStore;
use gradient_types::*;
use gradient_util::debounce::Debounce;
use gradient_util::nix_hash::normalize_nar_hash;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use uuid::Uuid;

async fn try_authenticate_basic(
    db: &WebDb,
    jwt_secret: &SecretString,
    last_used_stamps: &Debounce<Uuid>,
    headers: &HeaderMap,
    client_ip: IpAddr,
) -> WebResult<Option<(MUser, DecodedRequest)>> {
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
    let Ok(decoded) = decode_jwt(db, jwt_secret, last_used_stamps, password).await else {
        return Ok(None);
    };
    if let Some(ctx) = decoded.api_key_context()
        && !ip_allowed(client_ip, &ctx.allowed_ips)
    {
        return Err(SharedRefusal::SourceIpNotAllowed.into());
    }
    Ok(by_id::<EUser, _>(db, decoded.user_id())
        .await?
        .filter(|user| user.active)
        .map(|user| (user, decoded)))
}

async fn require_cache_auth(
    db: &WebDb,
    jwt_secret: &SecretString,
    last_used_stamps: &Debounce<Uuid>,
    headers: &HeaderMap,
    client_ip: IpAddr,
    cache: &MCache,
) -> WebResult<()> {
    if cache.public {
        return Ok(());
    }

    let Some((user, decoded)) =
        try_authenticate_basic(db, jwt_secret, last_used_stamps, headers, client_ip).await?
    else {
        return Err(CacheRefusal::AuthenticationRequired.into());
    };
    let api_key = decoded.api_key_context();
    if can_read_store(db, &user, cache, api_key).await? {
        return Ok(());
    }
    let unpinned_key = api_key.is_some_and(|key| key.cache_pin.is_none());
    if unpinned_key && can_read_store(db, &user, cache, None).await? {
        return Err(CacheRefusal::ApiKeyNotPinnedToCache.into());
    }
    Err(CacheRefusal::AuthenticationRequired.into())
}

async fn can_read_store(
    db: &WebDb,
    user: &MUser,
    cache: &MCache,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    Ok(effective_cache_mask(db, user.id, cache.id, api_key)
        .await?
        .is_some_and(|mask| cache_mask_grants(mask, CachePermission::ReadStore)))
}

pub(super) async fn get_nar_by_hash(
    db: &WebDb,
    config: &RuntimeConfig,
    cache: MCache,
    mapper: &StorePathMapper,
    hash: StoreHash,
) -> Result<NixPathInfo, WebError> {
    let hash = mapper.inbound_hash(hash);
    let cached_path_row = gradient_db::cached_paths::by_hash(db, hash.as_str())
        .await
        .map_err(WebError::from)?
        .filter(|row| row.is_fully_cached())
        .or_not_found(Resource::Path)?;

    let cached_path_sig =
        gradient_db::cached_paths::signature_in_cache(db, cached_path_row.id, cache.id)
            .await
            .map_err(WebError::from)?
            .or_not_found(Resource::Signature)?;

    let signature = cached_path_sig
        .signature
        .ok_or(CacheRefusal::SignaturePending)?;

    let sig =
        gradient_sources::full_signature_token(&signature, &config.server.serve_url, &cache.name);

    let file_hash = cached_path_row
        .file_hash
        .as_deref()
        .map(normalize_nar_hash)
        .ok_or(CacheRefusal::NotRecorded("FileHash"))?;
    let file_size = u32::try_from(
        cached_path_row
            .file_size
            .ok_or(CacheRefusal::NotRecorded("FileSize"))?
            .max(0),
    )
    .unwrap_or(u32::MAX);
    let nar_hash = cached_path_row
        .nar_hash
        .as_deref()
        .map(normalize_nar_hash)
        .ok_or(CacheRefusal::NotRecorded("NarHash"))?;
    let nar_size = cached_path_row
        .nar_size
        .ok_or(CacheRefusal::NotRecorded("NarSize"))? as u64;
    let references =
        gradient_db::graph::runtime_closure::references_for_hash(db, &cached_path_row.hash).await?;

    Ok(NixPathInfo {
        store_path: original_text(mapper, cached_path_row.store_path()),
        url: format!(
            "nar/{}.nar.zst",
            original_text(mapper, cached_path_row.hash.clone())
        ),
        compression: "zstd".to_string(),
        file_hash,
        file_size,
        nar_hash,
        nar_size,
        references: references
            .into_iter()
            .map(|reference| original_text(mapper, reference))
            .collect(),
        deriver: cached_path_row
            .deriver
            .map(|deriver| original_text(mapper, deriver)),
        sig,
        ca: cached_path_row.ca,
    })
}

pub(super) struct CacheContext {
    pub cache: MCache,
}

impl CacheContext {
    pub(super) async fn load(
        db: &WebDb,
        jwt_secret: &SecretString,
        last_used_stamps: &Debounce<Uuid>,
        headers: &HeaderMap,
        client_ip: IpAddr,
        cache_name: String,
    ) -> WebResult<Self> {
        let cache = gradient_db::caches::by_name(db, &cache_name)
            .await?
            .or_not_found(Resource::Cache)?;

        if !cache.active {
            return Err(CacheRefusal::CacheDisabled.into());
        }

        require_cache_auth(db, jwt_secret, last_used_stamps, headers, client_ip, &cache).await?;

        Ok(Self { cache })
    }
}

pub(super) fn cache_client_ip(
    config: &RuntimeConfig,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> IpAddr {
    let peer_ip = peer
        .map(|p| p.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    resolve_client_ip(headers, peer_ip, &config.network.trusted_proxies)
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JsonFlag {
    pub json: Option<String>,
}

impl JsonFlag {
    pub fn is_set(&self) -> bool {
        self.json.is_some()
    }
}

pub async fn fetch_nar_stream(
    db: &WebDb,
    nar_storage: &NarStore,
    cache: CacheId,
    stored_hash: &str,
) -> WebResult<(u64, BoxStream<'static, anyhow::Result<Bytes>>)> {
    if !gradient_db::cached_paths::signed_in_cache(db, cache, stored_hash).await? {
        return Err(WebError::not_found(Resource::Path));
    }
    let source = nar_storage
        .open(stored_hash, 0)
        .await
        .context("read NAR")?
        .or_not_found(Resource::Path)?;
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
    Ok((size, stream))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DeleteOutcome {
    pub ref_counted_others: bool,
}

pub(super) async fn delete_nar_from_cache(
    nar_storage: &NarStore,
    graph: &Arc<Graph>,
    cache_id: CacheId,
    hash: &str,
) -> WebResult<(MCachedPath, DeleteOutcome)> {
    let report = graph
        .demote(Demotion::CacheClaim {
            cache: cache_id,
            hash: hash.to_owned(),
        })
        .await
        .context("demote the cache claim")?;
    let cached_path = report.cached_path.or_not_found(Resource::Nar)?;
    nar_storage.hot().invalidate(hash);

    Ok((
        cached_path,
        DeleteOutcome {
            ref_counted_others: report.others_remain,
        },
    ))
}
