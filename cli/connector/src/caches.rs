/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct CacheResponse {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeCacheRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
}

pub async fn get(config: RequestConfig) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(config, "caches".to_string(), RequestType::GET, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await)
}

pub async fn put(
    config: RequestConfig,
    name: String,
    display_name: String,
    description: String,
    priority: i32,
) -> Result<BaseResponse<String>, String> {
    let req = MakeCacheRequest {
        name,
        display_name,
        description,
        priority,
    };

    let res = get_client(config, "caches".to_string(), RequestType::PUT, true)
        .unwrap()
        .json(&req)
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_cache(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<CacheResponse>, String> {
    let res = get_client(
        config,
        format!("caches/{}", organization),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn delete_cache(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("caches/{}", organization),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_cache_active(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("caches/{}/active", organization),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn delete_cache_active(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("caches/{}/active", organization),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
