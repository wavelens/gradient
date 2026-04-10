/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::decode_jwt;
use crate::error::WebError;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use core::executer::strip_nix_store_prefix;
use core::sources::get_path_from_derivation_output;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter};
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

/// Extracts HTTP Basic Auth credentials and resolves them to a user.
/// The password field is treated as a JWT or API key (the username is ignored).
pub(super) async fn try_authenticate_basic(
    headers: &HeaderMap,
    state: &Arc<ServerState>,
) -> Option<MUser> {
    let auth = headers.get(axum::http::header::AUTHORIZATION)?;
    let val = auth.to_str().ok()?;
    let encoded = val.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let creds = String::from_utf8(decoded).ok()?;
    let password = creds.split_once(':').map(|(_, p)| p)?.to_string();
    let token_data = decode_jwt(State(Arc::clone(state)), password).await.ok()?;
    EUser::find_by_id(token_data.claims.id)
        .one(&state.db)
        .await
        .ok()
        .flatten()
}

/// Returns true if `user` is allowed to read `cache`.
/// Access is granted when the user is the cache owner or belongs to any
/// organization that subscribes to the cache.
pub(super) async fn user_can_access_cache(
    state: &Arc<ServerState>,
    cache: &MCache,
    user: &MUser,
) -> bool {
    if cache.created_by == user.id {
        return true;
    }

    let org_ids: Vec<uuid::Uuid> = EOrganizationUser::find()
        .filter(COrganizationUser::User.eq(user.id))
        .all(&state.db)
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
        .one(&state.db)
        .await
        .unwrap_or(None)
        .is_some()
}

/// Checks authorization for a private cache request.
/// Returns `Ok(())` if the cache is public or if valid credentials grant access.
/// Returns `Err(401)` with a `WWW-Authenticate: Basic` challenge otherwise.
pub(super) async fn require_cache_auth(
    headers: &HeaderMap,
    state: &Arc<ServerState>,
    cache: &MCache,
) -> Result<(), (StatusCode, Json<BaseResponse<String>>)> {
    if cache.public {
        return Ok(());
    }

    let maybe_user = try_authenticate_basic(headers, state).await;
    match maybe_user {
        Some(user) if user_can_access_cache(state, cache, &user).await => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(BaseResponse {
                error: true,
                message: "Authentication required to access this cache".to_string(),
            }),
        )),
    }
}

pub(super) async fn get_nar_by_hash(
    state: Arc<ServerState>,
    cache: MCache,
    hash: String,
) -> Result<NixPathInfo, WebError> {
    let build_output = EDerivationOutput::find()
        .filter(
            Condition::all()
                .add(CDerivationOutput::IsCached.eq(true))
                .add(CDerivationOutput::Hash.eq(hash.clone())),
        )
        .one(&state.db)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found("Path"))?;

    // Verify the derivation belongs to an org that subscribes to this cache.
    let derivation = EDerivation::find_by_id(build_output.derivation)
        .one(&state.db)
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
        .one(&state.db)
        .await
        .map_err(WebError::from)?
        .is_some();

    if !subscribed {
        return Err(WebError::not_found("Path"));
    }

    let build_output_signature = EDerivationOutputSignature::find()
        .filter(
            Condition::all()
                .add(CDerivationOutputSignature::Cache.eq(cache.id))
                .add(CDerivationOutputSignature::DerivationOutput.eq(build_output.clone().id)),
        )
        .one(&state.db)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found("Signature"))?;

    let path = get_path_from_derivation_output(build_output.clone());

    let pathinfo = state
        .web_nix_store
        .query_pathinfo(path.to_string())
        .await
        .map_err(|e| {
            tracing::error!("Failed to get pathinfo: {}", e);
            WebError::InternalServerError("Failed to get path information".to_string())
        })?
        .ok_or_else(|| WebError::not_found("Path"))?;

    let nar_hash = normalize_nar_hash(&pathinfo.nar_hash);

    let references = pathinfo
        .references
        .into_iter()
        .map(|s| s.strip_prefix("/nix/store/").unwrap_or(&s).to_string())
        .collect();

    let sig_url = state
        .cli
        .serve_url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    let sig = format!(
        "{}-{}:{}",
        sig_url, cache.name, build_output_signature.signature
    );

    let file_hash = build_output
        .file_hash
        .ok_or_else(|| WebError::BadRequest("Missing file hash".to_string()))?;
    let file_hash_nix32 = file_hash.trim_start_matches("sha256:").to_string();

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
        nar_size: pathinfo.nar_size,
        references,
        deriver: pathinfo
            .deriver
            .map(|deriver| strip_nix_store_prefix(deriver.as_str())),
        sig,
        ca: pathinfo.ca,
    })
}

pub(super) async fn cleanup_nars_for_orgs(state: Arc<ServerState>, org_ids: Vec<Uuid>) {
    for org_id in org_ids {
        let remaining = EOrganizationCache::find()
            .filter(COrganizationCache::Organization.eq(org_id))
            .one(&state.db)
            .await
            .unwrap_or(None);

        if remaining.is_some() {
            continue;
        }

        let derivation_ids: Vec<Uuid> = EDerivation::find()
            .filter(CDerivation::Organization.eq(org_id))
            .all(&state.db)
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
            .all(&state.db)
            .await
            .unwrap_or_default();

        for output in outputs {
            if let Err(e) = state.nar_storage.delete(&output.hash).await {
                error!(error = %e, hash = %output.hash, "Failed to remove NAR from storage");
            }

            let mut active = output.into_active_model();
            active.is_cached = Set(false);
            if let Err(e) = active.update(&state.db).await {
                error!(error = %e, "Failed to update derivation_output is_cached flag");
            }
        }
    }
}

// ── Nix helpers ───────────────────────────────────────────────────────────────

pub(super) fn nix32_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let len = (bytes.len() * 8 - 1) / 5 + 1;
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let byte0 = bytes.get(i).copied().unwrap_or(0) as u32;
        let byte1 = bytes.get(i + 1).copied().unwrap_or(0) as u32;
        let c = ((byte0 >> j) | (byte1 << (8 - j))) & 0x1f;
        out.push(CHARS[c as usize] as char);
    }
    out
}

/// Converts any NarHash string (SRI `sha256-{base64}`, nix32 `sha256:{nix32}`, or bare hex)
/// to the narinfo wire format `sha256:{nix32}`.
pub(super) fn normalize_nar_hash(hash: &str) -> String {
    // SRI format: sha256-<base64>
    if let Some(b64) = hash.strip_prefix("sha256-")
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
    {
        return format!("sha256:{}", nix32_encode(&bytes));
    }
    // Already in nix32 format: sha256:<nix32>
    if hash.starts_with("sha256:") {
        return hash.to_string();
    }
    // Raw hex (64 chars = 32 bytes SHA-256)
    if hash.len() == 64
        && hash.chars().all(|c| c.is_ascii_hexdigit())
        && let Ok(bytes) = (0..32)
            .map(|i| u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
    {
        return format!("sha256:{}", nix32_encode(&bytes));
    }
    hash.to_string()
}
