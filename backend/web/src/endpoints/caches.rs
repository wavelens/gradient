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
use core::executer::{get_pathinfo, nix_store_path};
use core::input::{check_index_name, validate_display_name};
use core::sources::{
    format_cache_key, format_cache_public_key, generate_signing_key, get_cache_nar_compressed_location,
    get_cache_nar_location, get_hash_from_url, get_path_from_build_output,
};
use core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tracing::error;
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
    pub can_edit: bool,
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

    // Verify the build was produced by an org that subscribes to this cache.
    let build = EBuild::find_by_id(build_output.build)
        .one(&state.db)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found("Path"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found("Path"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await
            .map_err(WebError::from)?
            .ok_or_else(|| WebError::not_found("Path"))?;
        project.organization
    } else {
        let direct_build = EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
            .map_err(WebError::from)?
            .ok_or_else(|| WebError::not_found("Path"))?;
        direct_build.organization
    };

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
        deriver: pathinfo.deriver.map(|deriver| nix_store_path(deriver.as_str())),
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
    // Find all orgs the user belongs to
    let org_memberships = EOrganizationUser::find()
        .filter(COrganizationUser::User.eq(user.id))
        .all(&state.db)
        .await?;

    let org_ids: Vec<Uuid> = org_memberships.into_iter().map(|m| m.organization).collect();

    // Find cache IDs subscribed by those orgs
    let org_cache_ids: Vec<Uuid> = if org_ids.is_empty() {
        vec![]
    } else {
        EOrganizationCache::find()
            .filter(COrganizationCache::Organization.is_in(org_ids))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|oc| oc.cache)
            .collect()
    };

    let caches = ECache::find()
        .filter(
            Condition::any()
                .add(CCache::CreatedBy.eq(user.id))
                .add(CCache::Id.is_in(org_cache_ids)),
        )
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
        display_name: Set(body.display_name.trim().to_string()),
        description: Set(body.description.trim().to_string()),
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

    let can_edit = matches!(&maybe_user, Some(u) if u.id == cache.created_by);

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
            can_edit,
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
        let display_name = display_name.trim().to_string();
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
        acache.description = Set(description.trim().to_string());
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

pub async fn gradient_cache_info(
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

    let body = format!(
        "GradientVersion: {}\nGradientUrl: {}\n",
        env!("CARGO_PKG_VERSION"),
        state.cli.serve_url,
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/x-gradient-cache-info"),
        )
        .body(body)
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

    if let Ok(path_info) = get_nar_by_hash(Arc::clone(&state), cache.clone(), path_hash.clone()).await {
        return Response::builder()
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
            });
    }

    // Fall back: check external upstream caches.
    let upstreams = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.db)
        .await
        .unwrap_or_default();

    let http_client = reqwest::Client::new();
    for upstream in upstreams {
        let Some(ref base_url) = upstream.url else { continue };
        let narinfo_url = format!("{}/{}.narinfo", base_url.trim_end_matches('/'), path_hash);
        let Ok(resp) = http_client.get(&narinfo_url).send().await else { continue };
        if !resp.status().is_success() { continue; }
        let Ok(body) = resp.text().await else { continue };
        // Rewrite the URL: field to proxy through our upstream_nar endpoint.
        let rewritten = body
            .lines()
            .map(|line| {
                if let Some(nar_path) = line.strip_prefix("URL: ") {
                    format!("URL: nar/upstream/{}/{}", upstream.id, nar_path.trim())
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n") + "\n";
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/x-nix-narinfo"),
            )
            .body(rewritten)
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

    Err((
        StatusCode::NOT_FOUND,
        Json(BaseResponse {
            error: true,
            message: "Path not found".to_string(),
        }),
    ))
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

    // The URL uses the file hash (nix32 of compressed content).
    // Resolve it to the store hash so we can locate the on-disk NAR or pack path.
    let effective_hash = {
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
            output.hash
        } else {
            // Fallback: path_hash may itself be a store hash (legacy / direct hash URLs).
            path_hash.clone()
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

    let compressed_nar_path =
        get_cache_nar_compressed_location(state.cli.base_path.clone(), effective_hash.clone())
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Failed to get compressed cache location: {}", e),
                    }),
                )
            })?;

    let compressed = if tokio::fs::metadata(&compressed_nar_path).await.is_ok() {
        // Compressed NAR already cached on disk — serve directly.
        tokio::fs::read(&compressed_nar_path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to read compressed NAR: {}", e),
                }),
            )
        })?
    } else if tokio::fs::metadata(&nar_file_path).await.is_ok() {
        // Entry-point: raw NAR on disk — compress on the fly (no disk write needed,
        // entry-points are GC-rooted and always available).
        let nar_bytes = tokio::fs::read(&nar_file_path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to read NAR file: {}", e),
                }),
            )
        })?;
        tokio::task::spawn_blocking(move || zstd::bulk::compress(&nar_bytes, 3))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse { error: true, message: format!("Compression task panicked: {}", e) })))?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse { error: true, message: format!("Failed to compress NAR: {}", e) })))?
    } else {
        // Non-entry-point: pack from the nix store on the fly.
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

        let pack_path = if let Some(ref bo) = maybe_output {
            get_path_from_build_output(bo.clone())
        } else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Path not found".to_string(),
                }),
            ));
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

        let nar_bytes = output.stdout;
        let compressed_path_clone = compressed_nar_path.clone();
        let compressed =
            tokio::task::spawn_blocking(move || zstd::bulk::compress(&nar_bytes, 6))
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse { error: true, message: format!("Compression task panicked: {}", e) })))?
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse { error: true, message: format!("Failed to compress NAR: {}", e) })))?;

        // Persist compressed NAR so future requests skip the pack step.
        let to_write = compressed.clone();
        tokio::spawn(async move {
            if let Err(e) = tokio::fs::write(&compressed_path_clone, &to_write).await {
                tracing::warn!(error = %e, path = %compressed_path_clone, "Failed to write compressed NAR cache");
            }
        });

        compressed
    };

    let bytes_len = compressed.len() as i64;
    let cache_id = cache.id;
    let state_for_metric = Arc::clone(&state.0);
    tokio::spawn(async move {
        super::stats::record_nar_traffic(state_for_metric, cache_id, bytes_len).await;
    });

    // Update last_fetched_at for the served build_output (fire-and-forget).
    {
        let state_for_fetch = Arc::clone(&state.0);
        let hash = effective_hash.clone();
        tokio::spawn(async move {
            use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
            let now = chrono::Utc::now().naive_utc();
            let _ = state_for_fetch
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE build_output SET last_fetched_at = $1 WHERE hash = $2 AND is_cached = true",
                    [
                        sea_orm::Value::ChronoDateTimeUtc(Some(Box::new(chrono::DateTime::from_naive_utc_and_offset(now, chrono::Utc)))),
                        sea_orm::Value::String(Some(Box::new(hash))),
                    ],
                ))
                .await;
        });
    }

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

pub async fn upstream_nar(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache_name, upstream_id, path)): Path<(String, Uuid, String)>,
) -> Result<Response, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match ECache::find()
        .filter(CCache::Name.eq(cache_name))
        .one(&state.db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => return Err((StatusCode::NOT_FOUND, Json(BaseResponse { error: true, message: "Cache not found".to_string() }))),
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse { error: true, message: format!("Database error: {}", e) }))),
    };

    if !cache.active {
        return Err((StatusCode::BAD_REQUEST, Json(BaseResponse { error: true, message: "Cache is disabled".to_string() })));
    }

    require_cache_auth(&headers, &state, &cache).await?;

    let upstream = ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .one(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse { error: true, message: format!("Database error: {}", e) })))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(BaseResponse { error: true, message: "Upstream not found".to_string() })))?;

    let base_url = upstream.url.ok_or_else(|| (StatusCode::BAD_REQUEST, Json(BaseResponse { error: true, message: "Not an external upstream".to_string() })))?;

    let nar_url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    let http_client = reqwest::Client::new();
    let resp = http_client
        .get(&nar_url)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, Json(BaseResponse { error: true, message: format!("Upstream request failed: {}", e) })))?;

    if !resp.status().is_success() {
        return Err((StatusCode::NOT_FOUND, Json(BaseResponse { error: true, message: "Not found in upstream".to_string() })));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, Json(BaseResponse { error: true, message: format!("Failed to read upstream response: {}", e) })))?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("application/x-nix-nar"))
        .body(Body::from(bytes))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse { error: true, message: format!("Failed to build response: {}", e) })))
}

// ── Nix helpers ───────────────────────────────────────────────────────────────

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

/// Converts any NarHash string (SRI `sha256-{base64}`, nix32 `sha256:{nix32}`, or bare hex)
/// to the narinfo wire format `sha256:{nix32}`.
fn normalize_nar_hash(hash: &str) -> String {
    // SRI format: sha256-<base64>
    if let Some(b64) = hash.strip_prefix("sha256-")
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
            return format!("sha256:{}", nix32_encode(&bytes));
        }
    // Already in nix32 format: sha256:<nix32>
    if hash.starts_with("sha256:") {
        return hash.to_string();
    }
    // Raw hex (64 chars = 32 bytes SHA-256)
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())
        && let Ok(bytes) = (0..32)
            .map(|i| u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
        {
            return format!("sha256:{}", nix32_encode(&bytes));
        }
    hash.to_string()
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
