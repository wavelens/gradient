/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use core::types::*;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct NixCacheInfo {
    #[serde(rename = "WantMassQuery")]
    want_mass_query: bool,
    #[serde(rename = "StoreDir")]
    store_dir: String,
    #[serde(rename = "Priority")]
    priority: i32,
}

pub async fn get_nix_cache_info() -> Result<Json<NixCacheInfo>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority: 0,
    };

    Ok(Json(res))
}

pub async fn get_path(
    _state: State<Arc<ServerState>>,
    Path((_cache, _path)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
}

pub async fn get_nar(
    _state: State<Arc<ServerState>>,
    Path((_cache, _path)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
}

