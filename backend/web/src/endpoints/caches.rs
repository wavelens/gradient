/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeUser, decode_jwt, generate_api_key};
use crate::error::{WebError, WebResult};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::{Extension, Json};
use base64::Engine;
use chrono::{NaiveDateTime, Utc};
use core::database::{get_any_cache_by_name, get_cache_by_name};
use core::executer::get_pathinfo;
use core::input::{check_index_name, validate_display_name};
use core::sources::{
    clear_key, format_cache_key, format_cache_public_key, generate_signing_key,
    get_cache_nar_location, get_hash_from_url, get_path_from_build_output, sign_narinfo_fingerprint,
    write_key,
};
use core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use serde_json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Extracts HTTP Basic Auth credentials and resolves them to a user.
/// The password field is treated as a JWT or API key (the username is ignored).
async fn try_authenticate_basic(headers: &HeaderMap, state: &Arc<ServerState>) -> Option<MUser> {
    let auth = headers.get(axum::http::header::AUTHORIZATION)?;
    let val = auth.to_str().ok()?;
    let encoded = val.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
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
async fn user_can_access_cache(state: &Arc<ServerState>, cache: &MCache, user: &MUser) -> bool {
    if cache.created_by == user.id {
        return true;
    }

    let org_ids: Vec<Uuid> = EOrganizationUser::find()
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
async fn require_cache_auth(
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

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeCacheRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
    pub public: Option<bool>,
}

#[derive(Serialize)]
pub struct CacheResponse {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub public_key: String,
    pub public: bool,
    pub created_by: Uuid,
    pub created_at: NaiveDateTime,
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchCacheRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i32>,
}

async fn get_nar_by_hash(
    state: Arc<ServerState>,
    cache: MCache,
    hash: String,
) -> Result<NixPathInfo, WebError> {
    let build_output = EBuildOutput::find()
        .filter(
            Condition::all()
                .add(CBuildOutput::IsCached.eq(true))
                .add(CBuildOutput::Hash.eq(hash.clone())),
        )
        .one(&state.db)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found("Path"))?;

    let build_output_signature = EBuildOutputSignature::find()
        .filter(
            Condition::all()
                .add(CBuildOutputSignature::Cache.eq(cache.id))
                .add(CBuildOutputSignature::BuildOutput.eq(build_output.clone().id)),
        )
        .one(&state.db)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found("Signature"))?;

    let path = get_path_from_build_output(build_output.clone());

    let mut local_store = state.web_nix_store_pool.acquire().await.map_err(|e| {
        tracing::error!("Failed to acquire local store: {}", e);
        WebError::InternalServerError("Failed to access local store".to_string())
    })?;
    let pathinfo = get_pathinfo(path.to_string(), &mut *local_store)
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
        deriver: pathinfo.deriver,
        sig,
        ca: pathinfo.ca,
    })
}

pub async fn get_cache_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(Json(BaseResponse { error: false, message: false }));
    }
    let exists = ECache::find()
        .filter(CCache::Name.eq(name.as_str()))
        .one(&state.db)
        .await?
        .is_some();
    Ok(Json(BaseResponse {
        error: false,
        message: !exists,
    }))
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<MCache>>>> {
    // TODO: Implement pagination
    let caches = ECache::find()
        .filter(CCache::CreatedBy.eq(user.id))
        .all(&state.db)
        .await?;

    let res = BaseResponse {
        error: false,
        message: caches,
    };

    Ok(Json(res))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeCacheRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Cache Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
    }

    let existing_cache = ECache::find()
        .filter(CCache::Name.eq(body.name.clone()))
        .one(&state.db)
        .await?;

    if existing_cache.is_some() {
        return Err(WebError::already_exists("Cache Name"));
    }

    let (private_key, public_key) = generate_signing_key(state.cli.crypt_secret_file.clone())
        .map_err(|e| {
            tracing::error!("Failed to generate signing key: {}", e);
            WebError::InternalServerError("Failed to generate signing key".to_string())
        })?;

    let cache = ACache {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.clone()),
        description: Set(body.description.clone()),
        priority: Set(body.priority),
        public_key: Set(public_key),
        private_key: Set(private_key),
        public: Set(body.public.unwrap_or(false)),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
        managed: Set(false),
    };

    let cache = cache.insert(&state.db).await?;

    ACacheUpstream {
        id: Set(Uuid::new_v4()),
        cache: Set(cache.id),
        display_name: Set("cache.nixos.org".to_string()),
        mode: Set(CacheSubscriptionMode::ReadOnly),
        upstream_cache: Set(None),
        url: Set(Some("https://cache.nixos.org".to_string())),
        public_key: Set(Some(
            "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=".to_string(),
        )),
    }
    .insert(&state.db)
    .await?;

    let res = BaseResponse {
        error: false,
        message: cache.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_public_caches(
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<Vec<MCache>>>> {
    let caches = ECache::find()
        .filter(CCache::Public.eq(true))
        .all(&state.db)
        .await?;

    Ok(Json(BaseResponse {
        error: false,
        message: caches,
    }))
}

pub async fn get_cache(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<CacheResponse>>> {
    let cache: MCache = get_any_cache_by_name(state.0.clone(), cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public {
        match &maybe_user {
            Some(user) if cache.created_by == user.id => {}
            _ => return Err(WebError::not_found("Cache")),
        }
    }

    let public_key = format_cache_public_key(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!("Failed to derive public key: {}", e);
        WebError::InternalServerError("Failed to derive public key".to_string())
    })?;

    let res = BaseResponse {
        error: false,
        message: CacheResponse {
            id: cache.id,
            name: cache.name,
            display_name: cache.display_name,
            description: cache.description,
            active: cache.active,
            priority: cache.priority,
            public_key,
            public: cache.public,
            created_by: cache.created_by,
            created_at: cache.created_at,
            managed: cache.managed,
        },
    };

    Ok(Json(res))
}

pub async fn patch_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
    Json(body): Json<PatchCacheRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    // Prevent modification of state-managed caches
    if cache.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot modify state-managed cache. This cache is managed by configuration and cannot be edited through the API.".to_string(),
            }),
        ));
    }

    let mut acache: ACache = cache.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Cache Name".to_string(),
                }),
            ));
        }

        let cache = ECache::find()
            .filter(CCache::Name.eq(name.clone()))
            .one(&state.db)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Database error: {}", e),
                    }),
                )
            })?;

        if cache.is_some() {
            return Err((
                StatusCode::CONFLICT,
                Json(BaseResponse {
                    error: true,
                    message: "Cache Name already exists".to_string(),
                }),
            ));
        }

        acache.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        if let Err(e) = validate_display_name(&display_name) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: format!("Invalid display name: {}", e),
                }),
            ));
        }
        acache.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        acache.description = Set(description);
    }

    if let Some(priority) = body.priority {
        acache.priority = Set(priority);
    }

    if let Err(e) = acache.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to update cache: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Cache updated".to_string(),
    };

    Ok(Json(res))
}

async fn cleanup_nars_for_orgs(state: Arc<ServerState>, org_ids: Vec<Uuid>) {
    for org_id in org_ids {
        let remaining = EOrganizationCache::find()
            .filter(COrganizationCache::Organization.eq(org_id))
            .one(&state.db)
            .await
            .unwrap_or(None);

        if remaining.is_some() {
            continue;
        }

        let project_ids: Vec<Uuid> = EProject::find()
            .filter(CProject::Organization.eq(org_id))
            .all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.id)
            .collect();

        let eval_ids: Vec<Uuid> = EEvaluation::find()
            .filter(CEvaluation::Project.is_in(project_ids))
            .all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.id)
            .collect();

        let build_ids: Vec<Uuid> = EBuild::find()
            .filter(CBuild::Evaluation.is_in(eval_ids))
            .all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|b| b.id)
            .collect();

        let outputs = EBuildOutput::find()
            .filter(
                Condition::all()
                    .add(CBuildOutput::Build.is_in(build_ids))
                    .add(CBuildOutput::IsCached.eq(true)),
            )
            .all(&state.db)
            .await
            .unwrap_or_default();

        for output in outputs {
            if let Ok(nar_path) = get_cache_nar_location(state.cli.base_path.clone(), output.hash.clone())
                && let Err(e) = tokio::fs::remove_file(&nar_path).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!(error = %e, path = %nar_path, "Failed to remove NAR file");
            }

            let mut active = output.into_active_model();
            active.is_cached = Set(false);
            if let Err(e) = active.update(&state.db).await {
                error!(error = %e, "Failed to update build_output is_cached flag");
            }
        }
    }
}

pub async fn delete_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    // Prevent deletion of state-managed caches
    if cache.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot delete state-managed cache. This cache is managed by configuration and cannot be deleted through the API.".to_string(),
            }),
        ));
    }

    // Collect orgs that subscribe to this cache before deleting it, so we can
    // clean up orphaned NAR files in the background afterwards.
    let subscribing_orgs: Vec<Uuid> = EOrganizationCache::find()
        .filter(COrganizationCache::Cache.eq(cache.id))
        .all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|oc| oc.organization)
        .collect();

    let acache: ACache = cache.into();
    if let Err(e) = acache.delete(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to delete cache: {}", e),
            }),
        ));
    }

    // Spawn background task to delete now-orphaned NAR files.
    let state_bg = Arc::clone(&state);
    tokio::spawn(async move {
        cleanup_nars_for_orgs(state_bg, subscribing_orgs).await;
    });

    let res = BaseResponse {
        error: false,
        message: "Cache deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_cache_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    let mut acache: ACache = cache.into();
    acache.active = Set(true);
    if let Err(e) = acache.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to activate cache: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Cache enabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_cache_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    let mut acache: ACache = cache.into();
    acache.active = Set(false);
    if let Err(e) = acache.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to deactivate cache: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Cache disabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache: MCache = get_cache_by_name(state.0.clone(), user.id, cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if cache.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed cache.".to_string(),
        ));
    }

    let mut acache: ACache = cache.into();
    acache.public = Set(true);
    acache.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache is now public".to_string(),
    }))
}

pub async fn delete_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache: MCache = get_cache_by_name(state.0.clone(), user.id, cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if cache.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed cache.".to_string(),
        ));
    }

    let mut acache: ACache = cache.into();
    acache.public = Set(false);
    acache.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache is now private".to_string(),
    }))
}

pub async fn get_cache_key(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_any_cache_by_name(state.0.clone(), cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    if !cache.public && cache.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Cache not found".to_string(),
            }),
        ));
    }

    let cache_key = match format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache,
        state.cli.serve_url.clone(),
    ) {
        Ok(key) => key,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to generate cache key: {}", e),
                }),
            ));
        }
    };

    let res = BaseResponse {
        error: false,
        message: cache_key,
    };

    Ok(Json(res))
}

pub async fn get_cache_public_key(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_any_cache_by_name(state.0.clone(), cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    let allowed = cache.public || matches!(&maybe_user, Some(u) if u.id == cache.created_by);
    if !allowed {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Cache not found".to_string(),
            }),
        ));
    }

    let public_key = match format_cache_public_key(
        state.cli.crypt_secret_file.clone(),
        cache,
        state.cli.serve_url.clone(),
    ) {
        Ok(key) => key,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to derive public key: {}", e),
                }),
            ));
        }
    };

    Ok(Json(BaseResponse {
        error: false,
        message: public_key,
    }))
}

/// Returns a `.netrc` snippet for authenticating Nix against this cache.
///
/// Format:
/// ```text
/// machine <host>
/// login gradient
/// password GRAD<api_key>
/// ```
///
/// A dedicated API key named `netrc-<cache>` is created on first call and reused
/// on subsequent calls, so the returned credentials stay stable.
pub async fn get_cache_netrc(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = get_any_cache_by_name(state.0.clone(), cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public && !user_can_access_cache(&state, &cache, &user).await {
        return Err(WebError::not_found("Cache"));
    }

    let key_name = format!("netrc-{}", cache.name);

    let raw_key = match EApi::find()
        .filter(CApi::OwnedBy.eq(user.id))
        .filter(CApi::Name.eq(key_name.clone()))
        .one(&state.db)
        .await?
    {
        Some(existing) => existing.key,
        None => {
            let new_key = generate_api_key();
            AApi {
                id: Set(Uuid::new_v4()),
                owned_by: Set(user.id),
                name: Set(key_name),
                key: Set(new_key.clone()),
                last_used_at: Set(Utc::now().naive_utc()),
                created_at: Set(Utc::now().naive_utc()),
                managed: Set(false),
            }
            .insert(&state.db)
            .await?;
            new_key
        }
    };

    let host = state
        .cli
        .serve_url
        .replace("https://", "")
        .replace("http://", "")
        .split('/')
        .next()
        .unwrap_or("localhost")
        .to_string();

    let netrc = format!("machine {}\nlogin gradient\npassword GRAD{}\n", host, raw_key);

    Ok(Json(BaseResponse {
        error: false,
        message: netrc,
    }))
}

pub async fn nix_cache_info(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(cache): Path<String>,
) -> Result<Response<String>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match ECache::find()
        .filter(CCache::Name.eq(cache))
        .one(&state.db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    if !cache.active {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Cache is disabled".to_string(),
            }),
        ));
    }

    require_cache_auth(&headers, &state, &cache).await?;

    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority: cache.priority,
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/x-nix-cache-info"),
        )
        .body(res.to_nix_string())
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to build response: {}", e),
                }),
            )
        })
}

pub async fn path(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
) -> Result<Response<String>, (StatusCode, Json<BaseResponse<String>>)> {
    let path_hash = get_hash_from_url(path.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: e.to_string(),
            }),
        )
    })?;

    if !path.ends_with(".narinfo") {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Invalid path".to_string(),
            }),
        ));
    }

    let cache: MCache = match ECache::find()
        .filter(CCache::Name.eq(cache))
        .one(&state.db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    if !cache.active {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Cache is disabled".to_string(),
            }),
        ));
    }

    require_cache_auth(&headers, &state, &cache).await?;

    let path_info =
        match get_nar_by_hash(Arc::clone(&state), cache.clone(), path_hash.clone()).await {
            Ok(path_info) => path_info,
            Err(_) => {
                // Check the local nix store first — the path may have been substituted
                // during a previous narinfo request.
                if let Some(local_path) = find_store_path_by_hash(&path_hash).await {
                    match path_info_from_local_store(&state, &cache, &local_path).await
                    {
                        Ok(pi) => pi,
                        Err(e) => {
                            warn!(error = %e, hash = %path_hash, "path_info_from_local_store failed");
                            return Err((
                                StatusCode::NOT_FOUND,
                                Json(BaseResponse {
                                    error: true,
                                    message: "Path not found".to_string(),
                                }),
                            ));
                        }
                    }
                } else {
                    // Try each readable external upstream in order.
                    let upstreams = ECacheUpstream::find()
                        .filter(CCacheUpstream::Cache.eq(cache.id))
                        .all(&state.db)
                        .await
                        .unwrap_or_default();

                    let mut upstream_result: Option<NixPathInfo> = None;
                    for upstream in upstreams {
                        if upstream.mode == CacheSubscriptionMode::WriteOnly {
                            continue;
                        }
                        if let Some(ref url) = upstream.url {
                            match substitute_from_external_upstream(
                                &state,
                                &cache,
                                url,
                                upstream.public_key.as_deref(),
                                &path_hash,
                            )
                            .await
                            {
                                Ok(pi) => {
                                    upstream_result = Some(pi);
                                    break;
                                }
                                Err(e) => {
                                    warn!(
                                        error = %e,
                                        upstream = %url,
                                        hash = %path_hash,
                                        "Upstream substitution failed, trying next"
                                    );
                                }
                            }
                        }
                        // Internal upstream (another Gradient cache) — TODO: implement
                    }

                    match upstream_result {
                        Some(pi) => pi,
                        None => {
                            return Err((
                                StatusCode::NOT_FOUND,
                                Json(BaseResponse {
                                    error: true,
                                    message: "Path not found".to_string(),
                                }),
                            ));
                        }
                    }
                }
            }
        };

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/x-nix-narinfo"),
        )
        .body(path_info.to_nix_string())
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to build response: {}", e),
                }),
            )
        })
}

pub async fn nar(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
) -> Result<Response, (StatusCode, Json<BaseResponse<String>>)> {
    let path_hash = get_hash_from_url(path.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: e.to_string(),
            }),
        )
    })?;

    if !(path.ends_with(".nar") || path.contains(".nar.")) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Invalid path".to_string(),
            }),
        ));
    }

    let cache: MCache = match ECache::find()
        .filter(CCache::Name.eq(cache))
        .one(&state.db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    if !cache.active {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Cache is disabled".to_string(),
            }),
        ));
    }

    require_cache_auth(&headers, &state, &cache).await?;

    // Pull-through proxy: if this file hash was registered by substitute_from_external_upstream,
    // fetch bytes directly from the upstream and record traffic — no local nix store involved.
    if let Some(upstream_nar_url) = get_upstream_nar_proxy_map()
        .lock()
        .ok()
        .and_then(|m| m.get(&path_hash).cloned())
    {
        let proxy_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("HTTP client error: {}", e),
                    }),
                )
            })?;

        let proxy_resp = proxy_client
            .get(&upstream_nar_url)
            .send()
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Failed to fetch upstream NAR: {}", e),
                    }),
                )
            })?;

        if !proxy_resp.status().is_success() {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Upstream NAR not available".to_string(),
                }),
            ));
        }

        let nar_bytes = proxy_resp.bytes().await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to read upstream NAR: {}", e),
                }),
            )
        })?;

        let bytes_len = nar_bytes.len() as i64;
        let cache_id = cache.id;
        let state_for_metric = Arc::clone(&state.0);
        tokio::spawn(async move {
            super::stats::record_nar_traffic(state_for_metric, cache_id, bytes_len).await;
        });

        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-nix-nar"),
            )
            .body(Body::from(nar_bytes))
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Failed to build response: {}", e),
                    }),
                )
            });
    }

    // The URL now uses the file hash (nix32 of compressed content).
    // Resolve it to the store hash so we can locate the on-disk NAR or pack path.
    let (effective_hash, upstream_store_path) = {
        let by_file = EBuildOutput::find()
            .filter(
                Condition::all()
                    .add(CBuildOutput::IsCached.eq(true))
                    .add(CBuildOutput::FileHash.eq(format!("sha256:{}", path_hash))),
            )
            .one(&state.db)
            .await
            .map_err(WebError::from)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Database error: {}", e),
                    }),
                )
            })?;
        if let Some(output) = by_file {
            (output.hash, None)
        } else if let Some(sp) = get_upstream_map().lock().ok().and_then(|m| m.get(&path_hash).cloned()) {
            let h = sp.trim_start_matches("/nix/store/").split('-').next().unwrap_or("").to_string();
            (h, Some(sp))
        } else {
            // Fallback: treat path_hash as store hash (backward compatibility).
            (path_hash.clone(), None)
        }
    };

    let nar_file_path = get_cache_nar_location(state.cli.base_path.clone(), effective_hash.clone())
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to get cache location: {}", e),
                }),
            )
        })?;

    let nar_bytes = if tokio::fs::metadata(&nar_file_path).await.is_ok() {
        // Entry-point build: raw NAR is on disk — read and compress on the fly
        tokio::fs::read(&nar_file_path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to read NAR file: {}", e),
                }),
            )
        })?
    } else {
        // Try to find a DB record for a locally-built (cached) output.
        let maybe_output = EBuildOutput::find()
            .filter(
                Condition::all()
                    .add(CBuildOutput::IsCached.eq(true))
                    .add(CBuildOutput::Hash.eq(effective_hash.clone())),
            )
            .one(&state.db)
            .await
            .map_err(WebError::from)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Database error: {}", e),
                    }),
                )
            })?;

        let pack_path: String = if let Some(build_output) = maybe_output {
            // Non-entry-point build: pack from the nix store path in the DB record.
            get_path_from_build_output(build_output)
        } else if let Some(sp) = upstream_store_path {
            // Upstream-substituted path resolved from the file-hash map.
            sp
        } else {
            // Scan the local nix store for a matching path.
            match find_store_path_by_hash(&effective_hash).await {
                Some(p) => p,
                None => {
                    return Err((
                        StatusCode::NOT_FOUND,
                        Json(BaseResponse {
                            error: true,
                            message: "Path not found".to_string(),
                        }),
                    ));
                }
            }
        };

        let output = Command::new(state.cli.binpath_nix.clone())
            .arg("nar")
            .arg("pack")
            .arg(&pack_path)
            .output()
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Failed to pack NAR: {}", e),
                    }),
                )
            })?;

        if !output.status.success() {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: "nix nar pack failed".to_string(),
                }),
            ));
        }

        output.stdout
    };

    let compressed = tokio::task::spawn_blocking(move || zstd::bulk::compress(&nar_bytes, 3))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Compression task panicked: {}", e),
                }),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to compress NAR: {}", e),
                }),
            )
        })?;

    let bytes_len = compressed.len() as i64;
    let cache_id = cache.id;
    let state_for_metric = Arc::clone(&state.0);
    tokio::spawn(async move {
        super::stats::record_nar_traffic(state_for_metric, cache_id, bytes_len).await;
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        )
        .body(Body::from(compressed))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to build response: {}", e),
                }),
            )
        })
}

// ── Nix base-32 SHA-256 (mirrors the implementation in cache/src/cacher.rs) ──

fn nix32_encode(bytes: &[u8]) -> String {
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

fn nix_base32_sha256(data: &[u8]) -> String {
    let hash: [u8; 32] = Sha256::digest(data).into();
    nix32_encode(&hash)
}

/// Converts any NarHash string (SRI `sha256-{base64}`, nix32 `sha256:{nix32}`, or bare hex)
/// to the narinfo wire format `sha256:{nix32}`.
fn normalize_nar_hash(hash: &str) -> String {
    if let Some(b64) = hash.strip_prefix("sha256-") {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
            return format!("sha256:{}", nix32_encode(&bytes));
        }
    }
    hash.to_string()
}

// ── File-hash → store-path map for upstream-substituted paths ─────────────────

use std::sync::OnceLock;

fn get_upstream_map() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static MAP: OnceLock<std::sync::Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Maps `file_hash_nix32 → full upstream NAR URL` for pull-through paths that are
/// served by proxying the upstream directly (no local nix store involvement).
fn get_upstream_nar_proxy_map() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static PROXY_MAP: OnceLock<std::sync::Mutex<HashMap<String, String>>> = OnceLock::new();
    PROXY_MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

// ── Upstream pull-through helpers ─────────────────────────────────────────────

/// Scans `/nix/store` for a path whose hash prefix matches `hash`.
/// Returns the full store path (e.g. `/nix/store/{hash}-{name}`) if found.
async fn find_store_path_by_hash(hash: &str) -> Option<String> {
    let prefix = format!("{}-", hash);
    let mut dir = tokio::fs::read_dir("/nix/store").await.ok()?;
    while let Ok(Some(entry)) = dir.next_entry().await {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(&prefix) {
            return Some(entry.path().to_string_lossy().into_owned());
        }
    }
    None
}

/// Signs `store_path` with the cache's private key and returns the full signature token
/// (`{key_name}:{base64}`) ready to embed in a narinfo `Sig:` field.
async fn sign_and_get_sig(
    state: &ServerState,
    cache: &MCache,
    store_path: &str,
) -> Result<String, WebError> {
    let secret_key = format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
    )
    .map_err(|e| {
        WebError::InternalServerError(format!("Failed to format cache key: {}", e))
    })?;

    let key_file = write_key(secret_key.clone()).map_err(|e| {
        WebError::InternalServerError(format!("Failed to write key file: {}", e))
    })?;

    let sign_out = Command::new(&state.cli.binpath_nix)
        .arg("store")
        .arg("sign")
        .arg("-k")
        .arg(&key_file)
        .arg(store_path)
        .output()
        .await
        .map_err(|e| {
            WebError::InternalServerError(format!("Failed to run nix store sign: {}", e))
        })?;

    let _ = clear_key(key_file);

    if !sign_out.status.success() {
        return Err(WebError::InternalServerError(format!(
            "nix store sign failed: {}",
            String::from_utf8_lossy(&sign_out.stderr)
        )));
    }

    let sigs_out = Command::new(&state.cli.binpath_nix)
        .arg("path-info")
        .arg("--sigs")
        .arg(store_path)
        .output()
        .await
        .map_err(|e| {
            WebError::InternalServerError(format!("nix path-info --sigs failed: {}", e))
        })?;

    let signatures = String::from_utf8_lossy(&sigs_out.stdout);
    let key_name = secret_key.split(':').next().unwrap_or(&cache.name);

    for token in signatures.split_whitespace() {
        if token.starts_with(&format!("{}:", key_name)) {
            return Ok(token.trim().to_string());
        }
    }

    Err(WebError::InternalServerError(
        "Signature not found after signing".to_string(),
    ))
}

/// Builds a `NixPathInfo` for a path already in the local nix store.
/// Queries metadata via `nix path-info --json`, packs + compresses to compute file stats,
/// then signs with the cache's key.
async fn path_info_from_local_store(
    state: &Arc<ServerState>,
    cache: &MCache,
    store_path: &str,
) -> Result<NixPathInfo, WebError> {
    let json_out = Command::new(&state.cli.binpath_nix)
        .arg("path-info")
        .arg("--json")
        .arg(store_path)
        .output()
        .await
        .map_err(|e| {
            WebError::InternalServerError(format!("nix path-info --json failed: {}", e))
        })?;

    if !json_out.status.success() {
        return Err(WebError::InternalServerError(format!(
            "nix path-info failed: {}",
            String::from_utf8_lossy(&json_out.stderr)
        )));
    }

    let json: serde_json::Value = serde_json::from_slice(&json_out.stdout).map_err(|e| {
        WebError::InternalServerError(format!("Failed to parse path-info JSON: {}", e))
    })?;

    // Newer nix returns an array; older nix returns an object keyed by store path.
    let info = json
        .as_object()
        .and_then(|obj| obj.get(store_path).cloned())
        .or_else(|| {
            json.as_array().and_then(|arr| {
                arr.iter()
                    .find(|v| v.get("path").and_then(|p| p.as_str()) == Some(store_path))
                    .cloned()
            })
        })
        .ok_or_else(|| {
            WebError::InternalServerError(format!(
                "Store path {} not found in nix path-info output",
                store_path
            ))
        })?;

    let nar_hash = normalize_nar_hash(
        info.get("narHash")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let nar_size = info.get("narSize").and_then(|v| v.as_u64()).unwrap_or(0);
    let references: Vec<String> = info
        .get("references")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.as_str())
                .map(|s| s.strip_prefix("/nix/store/").unwrap_or(s).to_string())
                .collect()
        })
        .unwrap_or_default();
    let deriver = info
        .get("deriver")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.strip_prefix("/nix/store/").unwrap_or(s).to_string());
    let ca = info
        .get("ca")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Pack the NAR and compress to get file_hash / file_size.
    let pack_out = Command::new(&state.cli.binpath_nix)
        .arg("nar")
        .arg("pack")
        .arg(store_path)
        .output()
        .await
        .map_err(|e| WebError::InternalServerError(format!("nix nar pack failed: {}", e)))?;

    if !pack_out.status.success() {
        return Err(WebError::InternalServerError(format!(
            "nix nar pack failed: {}",
            String::from_utf8_lossy(&pack_out.stderr)
        )));
    }

    let stdout = pack_out.stdout;
    let compressed = tokio::task::spawn_blocking(move || zstd::bulk::compress(&stdout, 3))
        .await
        .map_err(|e| WebError::InternalServerError(format!("Compression task panicked: {}", e)))?
        .map_err(|e| WebError::InternalServerError(format!("zstd compression failed: {}", e)))?;

    let file_size = compressed.len() as u32;
    let file_hash_nix32 = nix_base32_sha256(&compressed);
    let file_hash = format!("sha256:{}", file_hash_nix32);
    let sig = sign_and_get_sig(state, cache, store_path).await?;

    // Store file-hash → store-path mapping so the nar handler can resolve it.
    if let Ok(mut map) = get_upstream_map().lock() {
        map.insert(file_hash_nix32.clone(), store_path.to_string());
    }

    Ok(NixPathInfo {
        store_path: store_path.to_string(),
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

/// Fetches the narinfo from an external upstream, signs it with the cache's own key,
/// and registers the NAR URL for HTTP proxying — without touching the local nix store.
///
/// This keeps pull-through traffic out of `total_stored` while still recording it in
/// the traffic metrics when the NAR is actually served.
async fn substitute_from_external_upstream(
    state: &Arc<ServerState>,
    cache: &MCache,
    upstream_url: &str,
    _trusted_public_key: Option<&str>,
    hash: &str,
) -> Result<NixPathInfo, WebError> {
    let narinfo_url = format!(
        "{}/{}.narinfo",
        upstream_url.trim_end_matches('/'),
        hash
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| {
            WebError::InternalServerError(format!("Failed to build HTTP client: {}", e))
        })?;

    let resp = client
        .get(&narinfo_url)
        .send()
        .await
        .map_err(|_| WebError::not_found("Path"))?;

    if !resp.status().is_success() {
        return Err(WebError::not_found("Path"));
    }

    let narinfo_text = resp.text().await.map_err(|e| {
        WebError::InternalServerError(format!("Failed to read upstream narinfo: {}", e))
    })?;

    // Helper: extract a single-line field value.
    fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
        let prefix = format!("{}: ", key);
        text.lines()
            .find(|l| l.starts_with(&prefix))
            .and_then(|l| l.split_once(": ").map(|x| x.1))
            .map(|s| s.trim())
    }

    let store_path = field(&narinfo_text, "StorePath")
        .ok_or_else(|| WebError::InternalServerError("No StorePath in upstream narinfo".to_string()))?
        .to_string();
    let nar_hash_raw = field(&narinfo_text, "NarHash")
        .ok_or_else(|| WebError::InternalServerError("No NarHash in upstream narinfo".to_string()))?
        .to_string();
    let nar_size: u64 = field(&narinfo_text, "NarSize")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| WebError::InternalServerError("No NarSize in upstream narinfo".to_string()))?;
    let upstream_nar_rel = field(&narinfo_text, "URL")
        .ok_or_else(|| WebError::InternalServerError("No URL in upstream narinfo".to_string()))?
        .to_string();
    let file_hash_raw = field(&narinfo_text, "FileHash")
        .ok_or_else(|| WebError::InternalServerError("No FileHash in upstream narinfo".to_string()))?
        .to_string();
    let file_size: u64 = field(&narinfo_text, "FileSize")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| WebError::InternalServerError("No FileSize in upstream narinfo".to_string()))?;
    let compression = field(&narinfo_text, "Compression").unwrap_or("xz").to_string();

    let references: Vec<String> = narinfo_text
        .lines()
        .find(|l| l.starts_with("References: "))
        .map(|l| l["References: ".len()..].trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| s.split_whitespace().map(|r| r.to_string()).collect())
        .unwrap_or_default();

    let deriver = field(&narinfo_text, "Deriver")
        .filter(|s| !s.is_empty() && *s != "unknown-deriver")
        .map(|s| s.to_string());
    let ca = field(&narinfo_text, "CA").map(|s| s.to_string());

    let nar_hash = normalize_nar_hash(&nar_hash_raw);

    // Sign narinfo fingerprint with our own key — no nix store involvement.
    let sig = sign_narinfo_fingerprint(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
        &store_path,
        &nar_hash,
        nar_size,
        &references,
    )
    .map_err(|e| WebError::InternalServerError(format!("Failed to sign narinfo: {}", e)))?;

    // Build full upstream NAR URL.
    let full_upstream_nar_url = if upstream_nar_rel.starts_with("http://")
        || upstream_nar_rel.starts_with("https://")
    {
        upstream_nar_rel.clone()
    } else {
        format!("{}/{}", upstream_url.trim_end_matches('/'), upstream_nar_rel)
    };

    let file_hash_nix32 = file_hash_raw.trim_start_matches("sha256:").to_string();

    // Register file-hash → upstream NAR URL so the nar handler can proxy it directly.
    if let Ok(mut map) = get_upstream_nar_proxy_map().lock() {
        map.insert(file_hash_nix32.clone(), full_upstream_nar_url);
    }

    info!(
        store_path = %store_path,
        upstream = %upstream_url,
        "Pull-through: registered upstream NAR proxy"
    );

    Ok(NixPathInfo {
        store_path,
        url: format!("nar/{}.nar.{}", file_hash_nix32, compression),
        compression,
        file_hash: file_hash_raw,
        file_size: file_size as u32,
        nar_hash,
        nar_size,
        references,
        deriver,
        sig,
        ca,
    })
}

// ── Upstream caches ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AddUpstreamRequest {
    /// An upstream that is another Gradient-managed cache (referenced by name).
    Internal {
        cache_name: String,
        display_name: Option<String>,
        mode: Option<CacheSubscriptionMode>,
    },
    /// An upstream that is an external Nix binary cache. Always ReadOnly.
    External {
        display_name: String,
        url: String,
        public_key: String,
    },
}

#[derive(Serialize)]
pub struct UpstreamCacheItem {
    pub id: Uuid,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    /// Set for internal upstreams.
    pub upstream_cache_id: Option<Uuid>,
    /// Set for external upstreams.
    pub url: Option<String>,
    pub public_key: Option<String>,
}

pub async fn get_cache_upstreams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<UpstreamCacheItem>>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let upstreams = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|u| UpstreamCacheItem {
            id: u.id,
            display_name: u.display_name,
            mode: u.mode,
            upstream_cache_id: u.upstream_cache,
            url: u.url,
            public_key: u.public_key,
        })
        .collect();

    Ok(Json(BaseResponse { error: false, message: upstreams }))
}

pub async fn put_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
    Json(body): Json<AddUpstreamRequest>,
) -> WebResult<Json<BaseResponse<Uuid>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let record = match body {
        AddUpstreamRequest::Internal { cache_name, display_name, mode } => {
            let upstream = get_cache_by_name(state.0.clone(), user.id, cache_name.clone())
                .await?
                .ok_or_else(|| WebError::not_found("Upstream cache"))?;
            if upstream.id == cache.id {
                return Err(WebError::BadRequest("A cache cannot be its own upstream".to_string()));
            }
            let name = display_name.unwrap_or_else(|| upstream.display_name.clone());
            ACacheUpstream {
                id: Set(Uuid::new_v4()),
                cache: Set(cache.id),
                display_name: Set(name),
                mode: Set(mode.unwrap_or(CacheSubscriptionMode::ReadWrite)),
                upstream_cache: Set(Some(upstream.id)),
                url: Set(None),
                public_key: Set(None),
            }
        }
        AddUpstreamRequest::External { display_name, url, public_key } => {
            ACacheUpstream {
                id: Set(Uuid::new_v4()),
                cache: Set(cache.id),
                display_name: Set(display_name),
                mode: Set(CacheSubscriptionMode::ReadOnly),
                upstream_cache: Set(None),
                url: Set(Some(url)),
                public_key: Set(Some(public_key)),
            }
        }
    };

    let inserted = record.insert(&state.db).await?;
    Ok(Json(BaseResponse { error: false, message: inserted.id }))
}

#[derive(Debug, Deserialize)]
pub struct PatchUpstreamRequest {
    pub display_name: Option<String>,
    pub mode: Option<CacheSubscriptionMode>,
    pub url: Option<String>,
    pub public_key: Option<String>,
}

pub async fn patch_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((cache, upstream_id)): Path<(String, Uuid)>,
    Json(body): Json<PatchUpstreamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let record = ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Upstream cache"))?;

    let is_external = record.upstream_cache.is_none();
    let mut active = record.into_active_model();

    if let Some(name) = body.display_name {
        active.display_name = Set(name);
    }
    if is_external {
        // External upstreams are always ReadOnly
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

    active.update(&state.db).await?;

    Ok(Json(BaseResponse { error: false, message: "Upstream updated".to_string() }))
}

pub async fn delete_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((cache, upstream_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let record = ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Upstream cache"))?;

    let active: ACacheUpstream = record.into();
    active.delete(&state.db).await?;

    Ok(Json(BaseResponse { error: false, message: "Upstream removed".to_string() }))
}
