/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeApiKeyRequest {
    pub name: String,
}

pub async fn get(config: RequestConfig) -> Result<BaseResponse<UserInfoResponse>, String> {
    let res = get_client(config, "user".to_string(), RequestType::Get, true)
        .unwrap()
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn delete(config: RequestConfig) -> Result<BaseResponse<String>, String> {
    let res = get_client(config, "user".to_string(), RequestType::Delete, true)
        .unwrap()
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn get_keys(config: RequestConfig) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(config, "user/keys".to_string(), RequestType::Get, true)
        .unwrap()
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn post_key(config: RequestConfig, name: String) -> Result<BaseResponse<String>, String> {
    let req = MakeApiKeyRequest { name };

    let res = get_client(config, "user/keys".to_string(), RequestType::Post, true)
        .unwrap()
        .json(&req)
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn delete_key(
    config: RequestConfig,
    name: String,
) -> Result<BaseResponse<String>, String> {
    let req = MakeApiKeyRequest { name };

    let res = get_client(config, "user/keys".to_string(), RequestType::Delete, true)
        .unwrap()
        .json(&req)
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await.unwrap())
}
