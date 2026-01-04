/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct OrganizationResponse {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: String,
    pub use_nix_store: bool,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeOrganizationRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct PatchOrganizationRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AddUserRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveUserRequest {
    pub user: String,
}

pub async fn get(config: RequestConfig) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(config, "orgs".to_string(), RequestType::GET, true)
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
) -> Result<BaseResponse<String>, String> {
    let req = MakeOrganizationRequest {
        name,
        display_name,
        description,
    };

    let res = get_client(config, "orgs".to_string(), RequestType::PUT, true)
        .unwrap()
        .json(&req)
        .send()
        .await
        .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_organization(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<OrganizationResponse>, String> {
    let res = get_client(
        config,
        format!("orgs/{}", organization),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn patch_organization(
    config: RequestConfig,
    organization: String,
    name: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
) -> Result<BaseResponse<String>, String> {
    let req = PatchOrganizationRequest {
        name,
        display_name,
        description,
    };

    let res = get_client(
        config,
        format!("orgs/{}", organization),
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

pub async fn delete_organization(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("orgs/{}", organization),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_organization_users(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/users", organization),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_organization_users(
    config: RequestConfig,
    organization: String,
    user: String,
    role: String,
) -> Result<BaseResponse<String>, String> {
    let req = AddUserRequest { user, role };

    let res = get_client(
        config,
        format!("orgs/{}/users", organization),
        RequestType::POST,
        true,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn patch_organization_users(
    config: RequestConfig,
    organization: String,
    user: String,
    role: String,
) -> Result<BaseResponse<String>, String> {
    let req = AddUserRequest { user, role };

    let res = get_client(
        config,
        format!("orgs/{}/users", organization),
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

pub async fn delete_organization_users(
    config: RequestConfig,
    organization: String,
    user: String,
) -> Result<BaseResponse<String>, String> {
    let req = RemoveUserRequest { user };

    let res = get_client(
        config,
        format!("orgs/{}/users", organization),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_organization_ssh(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/ssh", organization),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_organization_ssh(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/ssh", organization),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_organization_subscribe(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/subscribe", organization),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_organization_subscribe_cache(
    config: RequestConfig,
    organization: String,
    cache: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/subscribe/{}", organization, cache),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn delete_organization_subscribe_cache(
    config: RequestConfig,
    organization: String,
    cache: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/subscribe/{}", organization, cache),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
