/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
    pub use_nix_store: bool,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeOrganizationRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
}

pub async fn get(config: RequestConfig) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(config, "orgs".to_string(), RequestType::Get, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await)
}

pub async fn post(
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

    let res = get_client(config, "orgs".to_string(), RequestType::Post, true)
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
        RequestType::Get,
        true,
    )
    .unwrap()
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
        RequestType::Delete,
        true,
    )
    .unwrap()
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
        RequestType::Get,
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
        RequestType::Post,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
