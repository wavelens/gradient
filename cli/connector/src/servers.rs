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
    pub enabled: bool,
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
        RequestType::Get,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post(
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
        RequestType::Post,
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
        RequestType::Get,
        true,
    )
    .unwrap()
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
        RequestType::Delete,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_server_enable(
    config: RequestConfig,
    organization: String,
    server: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("servers/{}/{}/enable", organization, server),
        RequestType::Post,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_server_disable(
    config: RequestConfig,
    organization: String,
    server: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("servers/{}/{}/disable", organization, server),
        RequestType::Post,
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
        RequestType::Post,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
