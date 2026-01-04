/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct MakeUserRequest {
    /// Username (3-50 chars, alphanumeric/_/-, no consecutive special chars, not reserved)
    pub username: String,
    /// Full name of the user
    pub name: String,
    /// Valid email address
    pub email: String,
    /// Password (8-128 chars, must contain uppercase, lowercase, digit, and special char)
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct CheckUsernameRequest {
    pub username: String,
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
        RequestType::POST,
        false,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
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
        RequestType::POST,
        false,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_oauth_authorize(config: RequestConfig) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        "auth/oauth/authorize".to_string(),
        RequestType::GET,
        false,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_oauth_authorize(
    config: RequestConfig,
    code: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        "auth/oauth/authorize".to_string(),
        RequestType::POST,
        false,
    )
    .unwrap()
    .json(&code)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_logout(config: RequestConfig) -> Result<BaseResponse<String>, String> {
    let res = get_client(config, "auth/logout".to_string(), RequestType::POST, false)
        .unwrap()
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_check_username(
    config: RequestConfig,
    username: String,
) -> Result<BaseResponse<String>, String> {
    let req = CheckUsernameRequest { username };

    let res = get_client(
        config,
        "auth/check-username".to_string(),
        RequestType::POST,
        false,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_verify_email(
    config: RequestConfig,
    token: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("auth/verify-email?token={}", token),
        RequestType::GET,
        false,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_resend_verification(
    config: RequestConfig,
    username: String,
) -> Result<BaseResponse<String>, String> {
    let req = CheckUsernameRequest { username };

    let res = get_client(
        config,
        "auth/resend-verification".to_string(),
        RequestType::POST,
        false,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
