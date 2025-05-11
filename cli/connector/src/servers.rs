/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ServerResponse {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub organization: String,
    pub active: bool,
    pub host: String,
    pub port: i32,
    pub username: String,
    pub last_connection_at: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeServerRequest {
    pub name: String,
    pub display_name: String,
    pub host: String,
    pub port: i32,
    pub username: String,
    pub architectures: Vec<String>,
    pub features: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchServerRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub username: Option<String>,
    pub architectures: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeBuildRequest {
    pub log_streaming: bool,
}

pub async fn get(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(
        config,
        format!("servers/{}", organization),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn put(
    config: RequestConfig,
    organization: String,
    name: String,
    display_name: String,
    host: String,
    port: i32,
    ssh_user: String,
    architectures: Vec<String>,
    features: Vec<String>,
) -> Result<BaseResponse<String>, String> {
    let req = MakeServerRequest {
        name,
        display_name,
        host,
        port,
        username: ssh_user,
        architectures,
        features,
    };

    let res = get_client(
        config,
        format!("servers/{}", organization),
        RequestType::PUT,
        true,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_server(
    config: RequestConfig,
    organization: String,
    server: String,
) -> Result<BaseResponse<ServerResponse>, String> {
    let res = get_client(
        config,
        format!("servers/{}/{}", organization, server),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn patch_server(
    config: RequestConfig,
    organization: String,
    server: String,
    name: Option<String>,
    display_name: Option<String>,
    host: Option<String>,
    port: Option<i32>,
    ssh_user: Option<String>,
    architectures: Option<Vec<String>>,
    features: Option<Vec<String>>,
) -> Result<BaseResponse<String>, String> {
    let req = PatchServerRequest {
        name,
        display_name,
        host,
        port,
        username: ssh_user,
        architectures,
        features,
    };

    let res = get_client(
        config,
        format!("servers/{}/{}", organization, server),
        RequestType::PATCH,
        true,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn delete_server(
    config: RequestConfig,
    organization: String,
    server: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("servers/{}/{}", organization, server),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_server_active(
    config: RequestConfig,
    organization: String,
    server: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("servers/{}/{}/active", organization, server),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn delete_server_active(
    config: RequestConfig,
    organization: String,
    server: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("servers/{}/{}/active", organization, server),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_server_check_connection(
    config: RequestConfig,
    organization: String,
    server: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("servers/{}/{}/check-connection", organization, server),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
