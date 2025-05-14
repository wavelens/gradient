/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::{Extension, Json};
use axum::response::Response;
use axum::body::Body;
use chrono::Utc;
use core::database::get_cache_by_name;
use core::executer::{get_local_store, get_pathinfo};
use core::input::check_index_name;
use core::sources::{
    format_cache_key, generate_signing_key, get_hash_from_url, get_path_from_build_output, get_cache_nar_location
};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::process::Command;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeCacheRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
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
) -> Result<NixPathInfo, String> {
    let build_output = EBuildOutput::find()
        .filter(
            Condition::all()
                .add(CBuildOutput::IsCached.eq(true))
                .add(CBuildOutput::Hash.eq(hash.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if build_output.is_none() {
        return Err("Path not found".to_string());
    }

    let build_output = build_output.unwrap();
    let build_output_signature = EBuildOutputSignature::find()
        .filter(
            Condition::all()
                .add(CBuildOutputSignature::Cache.eq(cache.id))
                .add(CBuildOutputSignature::BuildOutput.eq(build_output.clone().id)),
        )
        .one(&state.db)
        .await
        .unwrap();

    if build_output_signature.is_none() {
        return Err("Signature not found".to_string());
    }

    let path = get_path_from_build_output(build_output.clone());

    let local_store = get_local_store(None).await.unwrap();
    let pathinfo = match local_store {
        LocalNixStore::UnixStream(mut store) => {
            get_pathinfo(path.to_string(), &mut store).await.unwrap()
        }
        LocalNixStore::CommandDuplex(mut store) => {
            get_pathinfo(path.to_string(), &mut store).await.unwrap()
        }
    };

    if pathinfo.is_none() {
        return Err("Path not found".to_string());
    }

    let pathinfo = pathinfo.unwrap();
    let output = match Command::new(state.cli.binpath_nix.clone())
        .arg("hash")
        .arg("convert")
        .arg("--from")
        .arg("base16")
        .arg("--to")
        .arg("nix32")
        .arg("--hash-algo")
        .arg("sha256")
        .arg(pathinfo.nar_hash)
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            return Err(format!("Failed to convert hash: {}", e));
        }
    };

    if !output.status.success() {
        return Err(format!(
            "Failed to convert hash: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let nar_hash = String::from_utf8_lossy(&output.stdout).to_string();

    Ok(NixPathInfo {
        store_path: path,
        url: format!("nar/{}.nar.zst", hash),
        compression: "zstd".to_string(),
        file_hash: build_output.file_hash.unwrap(),
        file_size: build_output.file_size.unwrap() as u32,
        nar_hash: format!("sha256:{}", nar_hash.trim()),
        nar_size: pathinfo.nar_size,
        references: pathinfo.references,
        sig: build_output_signature.unwrap().signature,
        ca: pathinfo.ca,
    })
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Implement pagination
    let caches = ECache::find()
        .filter(CCache::CreatedBy.eq(user.id))
        .all(&state.db)
        .await
        .unwrap();

    let caches: ListResponse = caches
        .iter()
        .map(|c| ListItem {
            id: c.id,
            name: c.name.clone(),
        })
        .collect();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Cache Name".to_string(),
            }),
        ));
    }

    let cache = ECache::find()
        .filter(CCache::Name.eq(body.name.clone()))
        .one(&state.db)
        .await
        .unwrap();

    if cache.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Cache Name already exists".to_string(),
            }),
        ));
    }

    let signing_key = match generate_signing_key(state.cli.crypt_secret_file.clone()) {
        Ok(key) => key,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: e.to_string(),
                }),
            ))
        }
    };

    let cache = ACache {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.clone()),
        description: Set(body.description.clone()),
        priority: Set(body.priority),
        signing_key: Set(signing_key),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let cache = cache.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: cache.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<MCache>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

    let res = BaseResponse {
        error: false,
        message: cache,
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
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

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
            .unwrap();

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
        acache.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        acache.description = Set(description);
    }

    if let Some(priority) = body.priority {
        acache.priority = Set(priority);
    }

    acache.update(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "Cache updated".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

    let acache: ACache = cache.into();
    acache.delete(&state.db).await.unwrap();

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
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

    let mut acache: ACache = cache.into();
    acache.active = Set(true);
    acache.update(&state.db).await.unwrap();

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
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

    let mut acache: ACache = cache.into();
    acache.active = Set(false);
    acache.update(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "Cache disabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_cache_key(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

    let res = BaseResponse {
        error: false,
        message: format_cache_key(
            state.cli.crypt_secret_file.clone(),
            cache,
            state.cli.serve_url.clone(),
            true,
        ),
    };

    Ok(Json(res))
}

pub async fn nix_cache_info(
    state: State<Arc<ServerState>>,
    Path(cache): Path<String>,
) -> Result<Response<String>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match ECache::find()
        .filter(CCache::Name.eq(cache))
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
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

    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority: cache.priority,
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/x-nix-cache-info"))
        .body(res.to_nix_string())
        .unwrap())
}

pub async fn path(
    state: State<Arc<ServerState>>,
    Path((cache, path)): Path<(String, String)>,
) -> Result<Response<String>, (StatusCode, Json<BaseResponse<String>>)> {
    let path_hash = get_hash_from_url(path.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: e,
            }),
        )
    });

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
        .unwrap()
    {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
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

    let path_info = match get_nar_by_hash(Arc::clone(&state), cache, path_hash.unwrap()).await {
        Ok(path_info) => path_info,
        Err(_) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Path not found".to_string(),
                }),
            ))
        }
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/x-nix-narinfo"))
        .body(path_info.to_nix_string())
        .unwrap())
}

pub async fn nar(
    state: State<Arc<ServerState>>,
    Path((cache, path)): Path<(String, String)>,
) -> Result<Response, (StatusCode, Json<BaseResponse<String>>)> {
    let path_hash = get_hash_from_url(path.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: e,
            }),
        )
    });

    if !path.ends_with(".nar") && !path.contains(".nar.") {
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
        .unwrap()
    {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
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

    let file_path = get_cache_nar_location(
        state.cli.base_path.clone(),
        path_hash.unwrap(),
        true,
    );

    let file = tokio::fs::File::open(&file_path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to open file: {}", e),
            }),
        )
    })?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("application/x-nix-nar"))
        .body(body)
        .unwrap())
}
