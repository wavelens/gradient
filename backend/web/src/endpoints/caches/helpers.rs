/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::decode_jwt;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use axum::extract::State;
use axum::http::HeaderMap;
use base64::Engine;
use gradient_core::nix_hash::normalize_nar_hash;
use gradient_core::sources::get_path_from_derivation_output;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::sync::Arc;
use tracing::error;

/// Extracts HTTP Basic Auth credentials and resolves them to a user.
/// The password field is treated as a JWT or API key (the username is ignored).
async fn try_authenticate_basic(state: &Arc<ServerState>, headers: &HeaderMap) -> Option<MUser> {
    let auth = headers.get(axum::http::header::AUTHORIZATION)?;
    let val = auth.to_str().ok()?;
    let encoded = val.strip_prefix("Basic ")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let creds = String::from_utf8(bytes).ok()?;
    let password = creds.split_once(':').map(|(_, p)| p)?.to_string();
    let decoded = decode_jwt(State(Arc::clone(state)), password).await.ok()?;
    EUser::find_by_id(decoded.user_id())
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
}

/// Returns true if `user` is allowed to read `cache`.
/// Access is granted when the user is the cache owner or belongs to any
/// organization that subscribes to the cache.
async fn user_can_access_cache(state: &Arc<ServerState>, cache: &MCache, user: &MUser) -> bool {
    if cache.created_by == user.id {
        return true;
    }

    let org_ids: Vec<OrganizationId> = EOrganizationUser::find()
        .filter(COrganizationUser::User.eq(user.id))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|ou| ou.organization)
        .collect();

    if org_ids.is_empty() {
        return false;
    }

    EOrganizationCache::find()
        .filter(COrganizationCache::Cache.eq(cache.id))
        .filter(COrganizationCache::Organization.is_in(org_ids))
        .one(&state.web_db)
        .await
        .unwrap_or(None)
        .is_some()
}

/// Checks authorization for a private cache request.
/// Returns `Ok(())` if the cache is public or if valid credentials grant access.
/// Returns `Err(Unauthorized)` otherwise.
async fn require_cache_auth(
    state: &Arc<ServerState>,
    headers: &HeaderMap,
    cache: &MCache,
) -> WebResult<()> {
    if cache.public {
        return Ok(());
    }

    let maybe_user = try_authenticate_basic(state, headers).await;
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

    // If there's no matching derivation_output, the requested hash may
    // belong to a `.drv` file or other standalone store path cached via
    // `cached_path`. Fall back to that lookup.
    let build_output = match build_output {
        Some(o) => o,
        None => return get_nar_by_cached_path(state, cache, hash).await,
    };

    // Verify the derivation belongs to an org that subscribes to this cache.
    let derivation = EDerivation::find_by_id(build_output.derivation)
        .one(&state.web_db)
        .await
        .map_err(WebError::from)?
        .or_not_found("Path")?;

    let organization_id = derivation.organization;

    let subscribed = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(organization_id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.web_db)
        .await
        .map_err(WebError::from)?
        .is_some();

    if !subscribed {
        return Err(WebError::not_found("Path"));
    }

    // Look up signature via cached_path → cached_path_signature for this cache.
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

    let path = get_path_from_derivation_output(build_output.clone());

    // All metadata comes from the cached_path row written by the worker
    // when it uploaded the NAR.  No daemon probe is needed.
    let nar_hash = cached_path_row
        .nar_hash
        .as_deref()
        .map(normalize_nar_hash)
        .or_not_found("NarHash not recorded")?;
    let nar_size = cached_path_row
        .nar_size
        .or_not_found("NarSize not recorded")? as u64;
    let references: Vec<String> = cached_path_row
        .references
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let deriver = cached_path_row.deriver.clone();
    let ca = cached_path_row.ca.clone();

    let sig = gradient_core::sources::full_signature_token(
        &signature,
        &state.config.server.serve_url,
        &cache.name,
    );

    // file_hash / file_size live on `cached_path` (written by the worker
    // during NarUploaded). The legacy mirror on `derivation_output` is
    // not always populated, so don't rely on it here.
    let file_hash = cached_path_row
        .file_hash
        .as_deref()
        .map(normalize_nar_hash)
        .or_not_found("FileHash not recorded")?;
    let file_hash_nix32 = file_hash.trim_start_matches("sha256:").to_string();
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

/// Narinfo lookup for store paths that aren't build outputs — notably
/// `.drv` files. Access is gated on the signature row for `cache.id`:
/// its existence proves the caller-authorised cache also holds the
/// path.  All metadata comes from `cached_path` because the server
/// local store may have GC'd the drv already.
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

    let sig = gradient_core::sources::full_signature_token(
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
    let references = cached_path_row
        .references
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let file_hash_nix32 = normalize_nar_hash(&file_hash)
        .trim_start_matches("sha256:")
        .to_string();

    Ok(NixPathInfo {
        store_path: cached_path_row.store_path.clone(),
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

pub(super) async fn cleanup_nars_for_orgs(state: Arc<ServerState>, org_ids: Vec<OrganizationId>) {
    cleanup_nars_for_orgs_inner(&state, org_ids).await
}

async fn cleanup_nars_for_orgs_inner(state: &Arc<ServerState>, org_ids: Vec<OrganizationId>) {
    for org_id in org_ids {
        let remaining = EOrganizationCache::find()
            .filter(COrganizationCache::Organization.eq(org_id))
            .one(&state.web_db)
            .await
            .unwrap_or(None);

        if remaining.is_some() {
            continue;
        }

        let derivation_ids: Vec<DerivationId> = EDerivation::find()
            .filter(CDerivation::Organization.eq(org_id))
            .all(&state.web_db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.id)
            .collect();

        let outputs = EDerivationOutput::find()
            .filter(
                Condition::all()
                    .add(CDerivationOutput::Derivation.is_in(derivation_ids))
                    .add(CDerivationOutput::IsCached.eq(true)),
            )
            .all(&state.web_db)
            .await
            .unwrap_or_default();

        for output in outputs {
            if let Err(e) = state.nar_storage.delete(&output.hash).await {
                error!(error = %e, hash = %output.hash, "Failed to remove NAR from storage");
            }

            let mut active = output.into_active_model();
            active.is_cached = Set(false);
            if let Err(e) = active.update(&state.web_db).await {
                error!(error = %e, "Failed to update derivation_output is_cached flag");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Resolved context for a Nix cache protocol request
// ---------------------------------------------------------------------------

/// Resolved context for a Nix cache protocol request.
///
/// Load with [`CacheContext::load`] which:
///  1. Looks up the cache by name
///  2. Rejects inactive caches with `BadRequest`
///  3. Enforces access control via `require_cache_auth`
pub(super) struct CacheContext {
    pub cache: MCache,
}

impl CacheContext {
    pub(super) async fn load(
        state: &Arc<ServerState>,
        headers: &HeaderMap,
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

        require_cache_auth(state, headers, &cache).await?;

        Ok(Self { cache })
    }
}
