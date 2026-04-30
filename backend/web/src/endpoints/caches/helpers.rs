/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::decode_jwt;
use crate::error::{WebError, WebResult};
use axum::extract::State;
use axum::http::HeaderMap;
use base64::Engine;
use core::nix_hash::normalize_nar_hash;
use core::sources::get_path_from_derivation_output;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

/// Bundles `&Arc<ServerState>` to avoid threading it through every cache-auth
/// and NAR-management helper as a free-function parameter.
struct CacheOpsHandler<'a> {
    state: &'a Arc<ServerState>,
}

impl<'a> CacheOpsHandler<'a> {
    fn new(state: &'a Arc<ServerState>) -> Self {
        Self { state }
    }

    /// Extracts HTTP Basic Auth credentials and resolves them to a user.
    /// The password field is treated as a JWT or API key (the username is ignored).
    async fn try_authenticate_basic(&self, headers: &HeaderMap) -> Option<MUser> {
        let auth = headers.get(axum::http::header::AUTHORIZATION)?;
        let val = auth.to_str().ok()?;
        let encoded = val.strip_prefix("Basic ")?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let creds = String::from_utf8(decoded).ok()?;
        let password = creds.split_once(':').map(|(_, p)| p)?.to_string();
        let token_data = decode_jwt(State(Arc::clone(self.state)), password)
            .await
            .ok()?;
        EUser::find_by_id(token_data.claims.id)
            .one(&self.state.db)
            .await
            .ok()
            .flatten()
    }

    /// Returns true if `user` is allowed to read `cache`.
    /// Access is granted when the user is the cache owner or belongs to any
    /// organization that subscribes to the cache.
    async fn user_can_access_cache(&self, cache: &MCache, user: &MUser) -> bool {
        if cache.created_by == user.id {
            return true;
        }

        let org_ids: Vec<uuid::Uuid> = EOrganizationUser::find()
            .filter(COrganizationUser::User.eq(user.id))
            .all(&self.state.db)
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
            .one(&self.state.db)
            .await
            .unwrap_or(None)
            .is_some()
    }

    /// Checks authorization for a private cache request.
    /// Returns `Ok(())` if the cache is public or if valid credentials grant access.
    /// Returns `Err(Unauthorized)` otherwise.
    async fn require_cache_auth(&self, headers: &HeaderMap, cache: &MCache) -> WebResult<()> {
        if cache.public {
            return Ok(());
        }

        let maybe_user = self.try_authenticate_basic(headers).await;
        match maybe_user {
            Some(user) if self.user_can_access_cache(cache, &user).await => Ok(()),
            _ => Err(WebError::Unauthorized(
                "Authentication required to access this cache".to_string(),
            )),
        }
    }

    async fn get_nar_by_hash(&self, cache: MCache, hash: String) -> Result<NixPathInfo, WebError> {
        let build_output = EDerivationOutput::find()
            .filter(
                Condition::all()
                    .add(CDerivationOutput::IsCached.eq(true))
                    .add(CDerivationOutput::Hash.eq(hash.clone())),
            )
            .one(&self.state.db)
            .await
            .map_err(WebError::from)?;

        // If there's no matching derivation_output, the requested hash may
        // belong to a `.drv` file or other standalone store path cached via
        // `cached_path`. Fall back to that lookup.
        let build_output = match build_output {
            Some(o) => o,
            None => return self.get_nar_by_cached_path(cache, hash).await,
        };

        // Verify the derivation belongs to an org that subscribes to this cache.
        let derivation = EDerivation::find_by_id(build_output.derivation)
            .one(&self.state.db)
            .await
            .map_err(WebError::from)?
            .ok_or_else(|| WebError::not_found("Path"))?;

        let organization_id = derivation.organization;

        let subscribed = EOrganizationCache::find()
            .filter(
                Condition::all()
                    .add(COrganizationCache::Organization.eq(organization_id))
                    .add(COrganizationCache::Cache.eq(cache.id)),
            )
            .one(&self.state.db)
            .await
            .map_err(WebError::from)?
            .is_some();

        if !subscribed {
            return Err(WebError::not_found("Path"));
        }

        // Look up signature via cached_path → cached_path_signature for this cache.
        let cached_path_row = ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash.clone()))
            .one(&self.state.db)
            .await
            .map_err(WebError::from)?
            .ok_or_else(|| WebError::not_found("CachedPath"))?;

        let cached_path_sig = ECachedPathSignature::find()
            .filter(
                Condition::all()
                    .add(CCachedPathSignature::CachedPath.eq(cached_path_row.id))
                    .add(CCachedPathSignature::Cache.eq(cache.id)),
            )
            .one(&self.state.db)
            .await
            .map_err(WebError::from)?
            .ok_or_else(|| WebError::not_found("Signature"))?;

        let signature = cached_path_sig
            .signature
            .ok_or_else(|| WebError::not_found("Signature not yet computed"))?;

        let path = get_path_from_derivation_output(build_output.clone());

        // All metadata comes from the cached_path row written by the worker
        // when it uploaded the NAR.  No daemon probe is needed.
        let nar_hash = cached_path_row
            .nar_hash
            .as_deref()
            .map(normalize_nar_hash)
            .ok_or_else(|| WebError::not_found("NarHash not recorded"))?;
        let nar_size = cached_path_row
            .nar_size
            .ok_or_else(|| WebError::not_found("NarSize not recorded"))?
            as u64;
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

        let sig_url = self
            .state
            .cli
            .serve_url
            .replace("https://", "")
            .replace("http://", "")
            .replace(":", "-");

        let sig = format!("{}-{}:{}", sig_url, cache.name, signature);

        let file_hash = build_output
            .file_hash
            .ok_or_else(|| WebError::BadRequest("Missing file hash".to_string()))?;
        let file_hash_nix32 = normalize_nar_hash(&file_hash)
            .trim_start_matches("sha256:")
            .to_string();

        Ok(NixPathInfo {
            store_path: path,
            url: format!("nar/{}.nar.zst", file_hash_nix32),
            compression: "zstd".to_string(),
            file_hash,
            file_size: build_output
                .file_size
                .ok_or_else(|| WebError::BadRequest("Missing file size".to_string()))?
                as u32,
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
        &self,
        cache: MCache,
        hash: String,
    ) -> Result<NixPathInfo, WebError> {
        let cached_path_row = ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash.clone()))
            .one(&self.state.db)
            .await
            .map_err(WebError::from)?
            .ok_or_else(|| WebError::not_found("Path"))?;

        if !cached_path_row.is_fully_cached() {
            return Err(WebError::not_found("Path"));
        }

        let cached_path_sig = ECachedPathSignature::find()
            .filter(
                Condition::all()
                    .add(CCachedPathSignature::CachedPath.eq(cached_path_row.id))
                    .add(CCachedPathSignature::Cache.eq(cache.id)),
            )
            .one(&self.state.db)
            .await
            .map_err(WebError::from)?
            .ok_or_else(|| WebError::not_found("Signature"))?;

        let signature = cached_path_sig
            .signature
            .ok_or_else(|| WebError::not_found("Signature not yet computed"))?;

        let sig_url = self
            .state
            .cli
            .serve_url
            .replace("https://", "")
            .replace("http://", "")
            .replace(":", "-");
        let sig = format!("{}-{}:{}", sig_url, cache.name, signature);

        let file_hash = cached_path_row
            .file_hash
            .clone()
            .ok_or_else(|| WebError::BadRequest("Missing file hash".to_string()))?;
        let file_size = cached_path_row
            .file_size
            .ok_or_else(|| WebError::BadRequest("Missing file size".to_string()))?
            as u32;
        let nar_hash = cached_path_row
            .nar_hash
            .as_deref()
            .map(normalize_nar_hash)
            .ok_or_else(|| WebError::not_found("NarHash not recorded"))?;
        let nar_size = cached_path_row
            .nar_size
            .ok_or_else(|| WebError::not_found("NarSize not recorded"))?
            as u64;
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

    async fn cleanup_nars_for_orgs(&self, org_ids: Vec<Uuid>) {
        for org_id in org_ids {
            let remaining = EOrganizationCache::find()
                .filter(COrganizationCache::Organization.eq(org_id))
                .one(&self.state.db)
                .await
                .unwrap_or(None);

            if remaining.is_some() {
                continue;
            }

            let derivation_ids: Vec<Uuid> = EDerivation::find()
                .filter(CDerivation::Organization.eq(org_id))
                .all(&self.state.db)
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
                .all(&self.state.db)
                .await
                .unwrap_or_default();

            for output in outputs {
                if let Err(e) = self.state.nar_storage.delete(&output.hash).await {
                    error!(error = %e, hash = %output.hash, "Failed to remove NAR from storage");
                }

                let mut active = output.into_active_model();
                active.is_cached = Set(false);
                if let Err(e) = active.update(&self.state.db).await {
                    error!(error = %e, "Failed to update derivation_output is_cached flag");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public(super) API — thin wrappers used by sibling modules
// ---------------------------------------------------------------------------

pub(super) async fn user_can_access_cache(
    state: &Arc<ServerState>,
    cache: &MCache,
    user: &MUser,
) -> bool {
    CacheOpsHandler::new(state)
        .user_can_access_cache(cache, user)
        .await
}

pub(super) async fn get_nar_by_hash(
    state: Arc<ServerState>,
    cache: MCache,
    hash: String,
) -> Result<NixPathInfo, WebError> {
    CacheOpsHandler::new(&state)
        .get_nar_by_hash(cache, hash)
        .await
}

pub(super) async fn cleanup_nars_for_orgs(state: Arc<ServerState>, org_ids: Vec<Uuid>) {
    CacheOpsHandler::new(&state)
        .cleanup_nars_for_orgs(org_ids)
        .await
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
            .one(&state.db)
            .await?
            .ok_or_else(|| WebError::not_found("Cache"))?;

        if !cache.active {
            return Err(WebError::BadRequest("Cache is disabled".to_string()));
        }

        CacheOpsHandler::new(state)
            .require_cache_auth(headers, &cache)
            .await?;

        Ok(Self { cache })
    }
}

