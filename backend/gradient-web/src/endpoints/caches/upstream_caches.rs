/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::CacheRefusal;
use crate::access::{
    AccessRefusal, CacheAccess, Caller, caller_cache_mask, load_cache, reject_managed_cache,
};
use crate::api_spec::CachePath;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::extractors::JsonBody;
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::CachePermission;
use crate::resource::Resource;
use crate::scope::{CacheScope, Readable, Requires, RequiresUnmanaged, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
pub use gradient_api_types::{
    AddUpstreamRequest, HttpUpstreamCredentialRequest, PatchHttpUpstreamCredentialRequest,
    PatchUpstreamRequest, UpstreamCacheItem, UpstreamTestResponse,
};
use gradient_core::ServerState;
use gradient_core::upstream_source::{credential_of_upstream_row, probe_protocol};
use gradient_db::WebDb;
use gradient_entity::cache_upstream::{
    CacheUpstreamKind, CacheUpstreamSource, UpstreamCredentialScheme,
};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::*;
use gradient_util::http::HttpProtocolSelection;
use gradient_util::http_base_url::{
    InvalidHttpBaseUrlError, UpstreamCacheUrlForDisplay, ValidatedHttpBaseUrl,
    upstream_cache_url_for_display_without_username_password_query_and_fragment,
    urls_differ_in_scheme_host_or_port,
};
use gradient_util::http_validation::UserUrlDestinationAllowlist;
use gradient_util::upstream_cache_credential::{
    upstream_cache_credential_plaintext_from_bearer_token,
    validate_basic_auth_and_make_upstream_cache_credential,
};
use sea_orm::ActiveValue::Set;
use sea_orm::{IntoActiveModel, TransactionTrait};
use std::sync::Arc;

fn validate_upstream_credential_request_and_return_scheme_and_plain_text_to_encrypt(
    request: &HttpUpstreamCredentialRequest,
) -> Result<(UpstreamCredentialScheme, String), CacheRefusal> {
    Ok(match request {
        HttpUpstreamCredentialRequest::Basic { user, password } => (
            UpstreamCredentialScheme::Basic,
            validate_basic_auth_and_make_upstream_cache_credential(user, password)?,
        ),
        HttpUpstreamCredentialRequest::Bearer { token } => (
            UpstreamCredentialScheme::Bearer,
            upstream_cache_credential_plaintext_from_bearer_token(token)?,
        ),
    })
}

fn upstream_cache_item_from_upstream_cache_row_and_optional_upstream_gradient_cache_name(
    upstream: MCacheUpstream,
    upstream_cache_name: Option<String>,
) -> UpstreamCacheItem {
    let shown_url = upstream
        .url
        .as_deref()
        .and_then(upstream_cache_url_for_display_without_username_password_query_and_fragment);
    UpstreamCacheItem {
        stored_url_has_user_password_or_query: shown_url
            .as_ref()
            .is_some_and(|shown| shown.had_removed_url_parts),
        url: shown_url.map(|UpstreamCacheUrlForDisplay { url, .. }| url),
        id: upstream.id,
        display_name: upstream.display_name,
        mode: upstream.mode,
        upstream_cache_id: upstream.upstream_cache,
        upstream_cache_name,
        public_key: upstream.public_key,
        kind: format!("{:?}", upstream.kind).to_lowercase(),
        remote_cache: upstream.remote_cache_name,
        http1_only: upstream.http1_only,
        active: upstream.active,
        credential_scheme: upstream.credential_scheme.map(Into::into),
    }
}

/// State is re-applying a managed cache's upstream caches on startup. Toggling `active` is the only
/// edit worth allowing there.
fn patch_edits_managed_fields(body: &PatchUpstreamRequest) -> bool {
    body.display_name.is_some()
        || body.mode.is_some()
        || body.url.is_some()
        || body.public_key.is_some()
        || body.http1_only.is_some()
        || body.credential.is_some()
}

fn validate_upstream_cache_url(url: &str) -> Result<ValidatedHttpBaseUrl, WebError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(CacheRefusal::UpstreamUrlRequired.into());
    }
    url.parse()
        .map_err(|e: InvalidHttpBaseUrlError| match e {
            InvalidHttpBaseUrlError::Userinfo => CacheRefusal::UpstreamCacheUrlContainsCredentials,
            InvalidHttpBaseUrlError::Query => CacheRefusal::CacheUrlContainsQueryString,
            other => CacheRefusal::UpstreamCacheUrlInvalid(other.to_string()),
        })
        .map_err(Into::into)
}

async fn check_upstream_cache_destination_allowed_and_host_has_usable_address(
    url: &ValidatedHttpBaseUrl,
    allowlist: &UserUrlDestinationAllowlist,
) -> Result<(), WebError> {
    allowlist
        .allowed_socket_addresses_for_url(url)
        .await
        .map(drop)
        .map_err(|e| CacheRefusal::UpstreamCacheUrlInvalid(e.to_string()).into())
}

fn validate_http(
    url: &str,
    public_key: &str,
    credential: Option<&HttpUpstreamCredentialRequest>,
) -> Result<
    (
        ValidatedHttpBaseUrl,
        Option<(UpstreamCredentialScheme, String)>,
    ),
    WebError,
> {
    let url = validate_upstream_cache_url(url)?;
    if public_key.trim().is_empty() {
        return Err(CacheRefusal::HttpPublicKeyRequired.into());
    }
    let payload = credential
        .map(validate_upstream_credential_request_and_return_scheme_and_plain_text_to_encrypt)
        .transpose()?;
    if payload.is_some() && !url.as_str().starts_with("https://") {
        return Err(CacheRefusal::UpstreamCacheCredentialSentOverPlainHttp.into());
    }
    Ok((url, payload))
}

fn validate_https_and_encrypt_upstream_cache_credential(
    crypt_file: &str,
    payload: Option<(UpstreamCredentialScheme, String)>,
) -> WebResult<(Option<String>, Option<UpstreamCredentialScheme>)> {
    let Some((scheme, payload)) = payload else {
        return Ok((None, None));
    };
    let encrypted = gradient_sources::encrypt_secret(crypt_file, &payload)
        .context("encrypt HTTP upstream credential")?;
    Ok((Some(encrypted), Some(scheme)))
}

fn validate_gradient_proto(
    url: &str,
    remote_cache: &str,
    api_key: Option<&str>,
) -> Result<ValidatedHttpBaseUrl, WebError> {
    let validated = validate_upstream_cache_url(url)?;
    if api_key.is_some_and(|k| !k.trim().is_empty()) && !validated.as_str().starts_with("https://")
    {
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
    Ok(validated)
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

async fn require_store_permissions_to_link_upstream_cache(
    db: &WebDb,
    user: &MUser,
    upstream: &MCache,
    mode: &CacheSubscriptionMode,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<()> {
    let needed: Vec<CachePermission> = [
        (
            *mode != CacheSubscriptionMode::WriteOnly && !upstream.public,
            CachePermission::ReadStore,
        ),
        (
            mode.writes_build_outputs_to_cache(),
            CachePermission::WriteStore,
        ),
    ]
    .into_iter()
    .filter_map(|(required, permission)| required.then_some(permission))
    .collect();
    if needed.is_empty() {
        return Ok(());
    }
    let mask = caller_cache_mask(db, Caller::User(user), upstream.id, api_key).await?;
    let missing: Vec<CachePermission> = needed
        .into_iter()
        .filter(|&permission| !crate::permissions::cache_mask_grants(mask, permission))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(AccessRefusal::MissingUpstreamCacheStorePermissions(missing).into())
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
    CacheScope {
        cache,
        db,
        user: maybe_user,
        api_key,
        ..
    }: CacheScope<Readable>,
) -> WebResult<Json<BaseResponse<Vec<UpstreamCacheItem>>>> {
    let upstreams = gradient_db::caches::upstream::of_cache(&db.web_db, cache.id).await?;
    let manages = crate::permissions::cache_mask_grants(
        caller_cache_mask(
            &db.web_db,
            Caller::from(&maybe_user),
            cache.id,
            api_key.as_ref(),
        )
        .await?,
        CachePermission::ManageUpstreamCaches,
    );
    let internal: Vec<CacheId> = upstreams.iter().filter_map(|u| u.upstream_cache).collect();
    let linked: std::collections::HashMap<CacheId, MCache> =
        gradient_db::lookup::by_ids::<ECache, _>(&db.web_db, &internal).await?;
    let upstream_caches = upstreams
        .into_iter()
        .map(|upstream| {
            let name = upstream
                .upstream_cache
                .and_then(|id| linked.get(&id))
                .filter(|target| manages || target.public)
                .map(|target| target.name.clone());
            upstream_cache_item_from_upstream_cache_row_and_optional_upstream_gradient_cache_name(
                upstream, name,
            )
        })
        .collect();

    Ok(ok_json(upstream_caches))
}

#[utoipa::path(
    put,
    path = "/caches/{cache}/upstream-caches",
    operation_id = "addUpstreamCache",
    tag = "caches",
    summary = "Add upstream cache",
    description = "Adds an upstream cache. Three types are supported, selected by the `type` discriminator:\n\n- **internal** - another Gradient-managed cache (referenced by name). Supports ReadOnly / ReadWrite / WriteOnly modes.\n- **gradient_proto** - a remote Gradient cache accessed via the binary WebSocket protocol (`/cache/{cache}/proto`). Supports optional `api_key` (stored encrypted at rest; write-only - never returned by the API) and pull mode.\n- **http** - an arbitrary Nix binary cache URL. Always ReadOnly. Supports an optional `credential` (`basic` with `user` and `password`, or `bearer` with `token`) over https:// only. The credential is stored encrypted at rest and is write-only: the API returns only `credential_scheme`. The URL can't contain a username, password or query. Workers never fetch from an upstream cache with a credential. Such an upstream cache serves only this cache's pull-through, build logs and the Test route.\n\nCallers need `manageUpstreamCaches` for this cache. Callers also need `writeStore` for the upstream cache in ReadWrite mode or WriteOnly mode to link an internal upstream cache. Callers need `readStore` for the upstream cache in ReadOnly mode or ReadWrite mode unless the upstream cache is public.",
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
    JsonBody(body): JsonBody<AddUpstreamRequest>,
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
            let mode = mode.unwrap_or(CacheSubscriptionMode::ReadWrite);
            require_store_permissions_to_link_upstream_cache(
                &db.web_db,
                &user,
                &upstream,
                &mode,
                api_key.as_ref(),
            )
            .await?;
            let name = display_name.unwrap_or_else(|| upstream.display_name.clone());
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: cache.id,
                display_name: name,
                mode,
                kind: CacheUpstreamKind::Internal,
                upstream_cache: Some(upstream.id),
                ..Default::default()
            }
        }
        AddUpstreamRequest::Http {
            display_name,
            url,
            public_key,
            http1_only,
            credential,
        } => {
            let (url, payload) = validate_http(&url, &public_key, credential.as_ref())?;
            check_upstream_cache_destination_allowed_and_host_has_usable_address(
                &url,
                db.user_supplied_url_http_client.allowlist(),
            )
            .await?;
            let (encrypted_credential, credential_scheme) =
                validate_https_and_encrypt_upstream_cache_credential(
                    &state.config.secrets.crypt_file,
                    payload,
                )?;
            MCacheUpstream {
                id: CacheUpstreamId::now_v7(),
                cache: cache.id,
                display_name,
                mode: CacheSubscriptionMode::ReadOnly,
                kind: CacheUpstreamKind::Http,
                url: Some(url.to_string()),
                public_key: Some(public_key),
                http1_only,
                encrypted_credential,
                credential_scheme,
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
            let url = validate_gradient_proto(&url, &remote_cache, key.as_deref())?;
            check_upstream_cache_destination_allowed_and_host_has_usable_address(
                &url,
                db.user_supplied_url_http_client.allowlist(),
            )
            .await?;
            let encrypted_credential = match key {
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
                url: Some(url.to_string()),
                remote_cache_name: Some(remote_cache.trim().to_string()),
                credential_scheme: encrypted_credential
                    .as_ref()
                    .map(|_| UpstreamCredentialScheme::Bearer),
                encrypted_credential,
                ..Default::default()
            }
        }
    };

    let inserted =
        gradient_db::caches::upstream::insert_upstream_cache_if_below_max_upstream_count(
            &state.web_db,
            record,
        )
        .await?
        .ok_or(CacheRefusal::MaximumUpstreamCacheCountReached)?;
    Ok(ok_json(inserted.id))
}

#[utoipa::path(
    patch,
    path = "/caches/{cache}/upstream-caches/{id}",
    operation_id = "patchUpstreamCache",
    tag = "caches",
    summary = "Update upstream cache",
    description = "Updates an upstream entry. For external upstream caches the mode is always forced to ReadOnly;\n`url`, `public_key`, `http1_only` and `credential` may be updated. `credential` takes `basic` (`user`, `password`) or `bearer` (`token`) to set or replace the stored credential, or `none` to remove it. A credential needs an https:// URL. Moving the URL to another scheme, host or port while a credential is stored needs the credential again in the same request, or its removal. For internal upstream caches only `mode` and `display_name` apply.\n`active` toggles any upstream cache. An inactive upstream cache is kept but never queried. Setting `active` to true checks the stored URL again and refuses a URL with a username, password or query.\nA state-managed cache accepts only `active`, and the next server start restores the declared value.\nCallers need `manageUpstreamCaches` for this cache. Callers also need `writeStore` for the upstream cache in ReadWrite mode or WriteOnly mode to link an internal upstream cache. Callers need `readStore` for the upstream cache in ReadOnly mode or ReadWrite mode unless the upstream cache is public.",
    params(CachePath, ("id" = CacheUpstreamId, Path)),
    request_body = PatchUpstreamRequest,
    responses((status = OK, description = "Updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_cache_upstream(
    CacheScope {
        cache,
        user,
        api_key,
        db,
        params,
        ..
    }: CacheScope<Requires<can::ManageUpstreamCaches>>,
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<PatchUpstreamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let upstream_id: CacheUpstreamId = params.param("id")?;

    if patch_edits_managed_fields(&body) {
        reject_managed_cache(&cache)?;
    }
    let record = load_upstream(&db.web_db, cache.id, upstream_id).await?;
    let is_external = matches!(record.as_source(), Some(CacheUpstreamSource::Http { .. }));
    let request_replaces_external_http_cache_url = is_external && body.url.is_some();

    if body.active == Some(true)
        && !request_replaces_external_http_cache_url
        && let Some(stored_url) = record.url.as_deref()
    {
        validate_upstream_cache_url(stored_url)?;
    }

    if let (Some(upstream_cache), Some(mode)) = (record.upstream_cache, &body.mode) {
        let upstream = gradient_db::lookup::by_id::<ECache, _>(&db.web_db, upstream_cache)
            .await?
            .or_not_found(Resource::Cache)?;
        require_store_permissions_to_link_upstream_cache(
            &db.web_db,
            &user,
            &upstream,
            mode,
            api_key.as_ref(),
        )
        .await?;
    }

    let stored_url = record.url.clone().unwrap_or_default();
    let has_stored_credential = record.encrypted_credential.is_some();
    let mut row = record.into_active_model();
    let mut should_clear_stored_nar_download_urls = false;

    if let Some(name) = body.display_name {
        row.display_name = Set(name);
    }
    if let Some(enabled) = body.active {
        row.active = Set(enabled);
    }
    if is_external {
        row.mode = Set(CacheSubscriptionMode::ReadOnly);
        let new_url = body
            .url
            .as_deref()
            .map(validate_upstream_cache_url)
            .transpose()?;
        let credential_change = body
            .credential
            .map(PatchHttpUpstreamCredentialRequest::into_credential_request_to_store);
        let payload = credential_change
            .as_ref()
            .and_then(Option::as_ref)
            .map(validate_upstream_credential_request_and_return_scheme_and_plain_text_to_encrypt)
            .transpose()?;
        let keeps_stored_credential = has_stored_credential && credential_change.is_none();
        let target_url = new_url
            .as_ref()
            .map_or(stored_url.as_str(), ValidatedHttpBaseUrl::as_str);
        let sends_credential_to_target =
            payload.is_some() || (keeps_stored_credential && new_url.is_some());
        if sends_credential_to_target && !target_url.starts_with("https://") {
            return Err(CacheRefusal::UpstreamCacheCredentialSentOverPlainHttp.into());
        }
        if keeps_stored_credential
            && new_url.is_some()
            && urls_differ_in_scheme_host_or_port(&stored_url, target_url)
        {
            return Err(CacheRefusal::StoredCredentialWouldBeSentToNewUpstreamCacheHost.into());
        }
        should_clear_stored_nar_download_urls = new_url.is_some() || credential_change.is_some();
        if let Some(url) = new_url {
            check_upstream_cache_destination_allowed_and_host_has_usable_address(
                &url,
                db.user_supplied_url_http_client.allowlist(),
            )
            .await?;
            row.url = Set(Some(url.to_string()));
        }
        if credential_change.is_some() {
            let (encrypted_credential, credential_scheme) =
                validate_https_and_encrypt_upstream_cache_credential(
                    &state.config.secrets.crypt_file,
                    payload,
                )?;
            row.encrypted_credential = Set(encrypted_credential);
            row.credential_scheme = Set(credential_scheme);
        }
        if let Some(key) = body.public_key {
            if key.trim().is_empty() {
                return Err(CacheRefusal::HttpPublicKeyRequired.into());
            }
            row.public_key = Set(Some(key));
        }
        if let Some(http1_only) = body.http1_only {
            row.http1_only = Set(http1_only);
        }
    } else if let Some(mode) = body.mode {
        row.mode = Set(mode);
    }

    let tx = db.web_db.inner().begin().await?;
    gradient_db::rows::update::<ECacheUpstream, _>(&tx, row).await?;
    if should_clear_stored_nar_download_urls {
        gradient_db::caches::upstream::clear_nar_url_and_upstream_reference_for_build_outputs_verified_by_upstream(&tx, upstream_id)
            .await?;
    }
    tx.commit().await?;

    Ok(ok_json("Upstream updated".to_string()))
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/upstream-caches/{id}/test",
    operation_id = "testUpstreamCache",
    tag = "caches",
    summary = "Test an HTTP upstream cache over HTTP/1.1 and HTTP/2",
    description = "Fetches the upstream cache's `nix-cache-info` twice in parallel, once over HTTP/1.1 only and once\nover HTTP/2 only, and reports each result. A probe passes on a 2xx body with a `StoreDir:` line.\nDoes not change the upstream cache's `http1_only` setting. Only `http` upstream caches can be tested.\nRequires `manageUpstreamCaches`; state-managed caches may be tested.",
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

    let url: ValidatedHttpBaseUrl =
        url.parse()
            .map_err(|e: gradient_util::http_base_url::InvalidHttpBaseUrlError| {
                CacheRefusal::UpstreamCacheUrlInvalid(e.to_string())
            })?;
    let credential = credential_of_upstream_row(&db.config.secrets.crypt_file, &record)
        .await
        .context("decrypt upstream cache credential")?;
    let http = &db.user_supplied_url_http_client;
    let (http1, http2) = tokio::join!(
        probe_protocol(
            http,
            &url,
            HttpProtocolSelection::Http1Only,
            credential.as_deref()
        ),
        probe_protocol(
            http,
            &url,
            HttpProtocolSelection::Http2PriorKnowledge,
            credential.as_deref()
        ),
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
            serde_json::json!({ "http1_only": true }),
            serde_json::json!({ "credential": { "scheme": "none" } }),
        ] {
            assert!(patch_edits_managed_fields(&patch(edit)));
        }
    }

    #[test]
    fn patch_upstream_request_debug_omits_credential_secret() {
        for credential in [
            serde_json::json!({ "scheme": "basic", "user": "alice", "password": "pw-hidden" }),
            serde_json::json!({ "scheme": "bearer", "token": "pw-hidden" }),
        ] {
            let patch = serde_json::from_value::<PatchUpstreamRequest>(
                serde_json::json!({ "credential": credential }),
            )
            .unwrap();
            let debug = format!("{patch:?}");
            assert!(!debug.contains("pw-hidden"), "{debug}");
            assert!(!debug.contains("alice"), "{debug}");
        }
    }

    fn user_url_destination_allowlist_with_dns_lookup_function(
        lookup: gradient_util::http_validation::HostNameAndPortToSocketAddressesLookupFunction,
    ) -> UserUrlDestinationAllowlist {
        UserUrlDestinationAllowlist::default().with_dns_lookup_function(lookup)
    }

    #[test]
    fn validate_http_requires_url_and_key() {
        assert!(validate_http("", "k", None).is_err());
        assert!(validate_http("https://x", "", None).is_err());
        assert!(validate_http("not a url", "k", None).is_err());
        assert!(validate_http("https://cache.nixos.org", "cache.nixos.org-1:abc", None).is_ok());
    }

    #[tokio::test]
    async fn upstream_cache_url_rejects_internal_addresses() {
        for url in [
            "http://169.254.169.254",
            "http://localhost:8080",
            "http://127.0.0.1/nix-cache",
            "http://10.0.0.5",
            "http://[::1]:5000",
        ] {
            let parsed = validate_upstream_cache_url(url).unwrap();
            assert!(
                check_upstream_cache_destination_allowed_and_host_has_usable_address(
                    &parsed,
                    &Default::default()
                )
                .await
                .is_err(),
                "{url}"
            );
        }
        let public = user_url_destination_allowlist_with_dns_lookup_function(|_, port| {
            Box::pin(async move {
                Ok(vec![std::net::SocketAddr::new(
                    [203, 0, 113, 8].into(),
                    port,
                )])
            })
        });
        let parsed = validate_upstream_cache_url(" https://cache.nixos.org ").unwrap();
        assert!(
            check_upstream_cache_destination_allowed_and_host_has_usable_address(&parsed, &public)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn creating_upstream_cache_of_any_kind_is_refused_when_dns_answer_has_zero_addresses() {
        let empty = user_url_destination_allowlist_with_dns_lookup_function(|_, _| {
            Box::pin(async { Ok(Vec::new()) })
        });
        for url in [
            validate_http("https://empty.example/", "cache-1:abc", None)
                .unwrap()
                .0,
            validate_gradient_proto("https://empty.example/", "prod", None).unwrap(),
        ] {
            let refusal =
                check_upstream_cache_destination_allowed_and_host_has_usable_address(&url, &empty)
                    .await
                    .expect_err("a host without addresses is refused");
            let text = format!("{refusal} {refusal:?}");
            assert!(
                text.contains("The server cannot connect to empty.example"),
                "{text}"
            );
        }
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
