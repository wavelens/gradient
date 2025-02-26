/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct MakeUserRequest {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
}

pub async fn post_basic_register(
    config: RequestConfig,
    username: String,
    name: String,
    email: String,
    password: String,
) -> Result<BaseResponse<String>, String> {
    let req = MakeUserRequest {
        username,
        name,
        email,
        password,
    };

    let res = get_client(
        config,
        "auth/basic/register".to_string(),
        RequestType::Post,
        false,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn post_basic_login(
    config: RequestConfig,
    loginname: String,
    password: String,
) -> Result<BaseResponse<String>, String> {
    let req = MakeLoginRequest {
        loginname,
        password,
    };

    let res = get_client(
        config,
        "auth/basic/login".to_string(),
        RequestType::Post,
        false,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn get_oauth_authorize(config: RequestConfig) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        "auth/oauth/authorize".to_string(),
        RequestType::Get,
        false,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn post_oauth_authorize(
    config: RequestConfig,
    code: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        "auth/oauth/authorize".to_string(),
        RequestType::Post,
        false,
    )
    .unwrap()
    .json(&code)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn post_logout(config: RequestConfig) -> Result<BaseResponse<String>, String> {
    let res = get_client(config, "auth/logout".to_string(), RequestType::Post, false)
        .unwrap()
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await.unwrap())
}
