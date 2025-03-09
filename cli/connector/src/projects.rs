/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectResponse {
    pub id: String,
    pub organization: String,
    pub name: String,
    pub active: bool,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
    pub last_evaluation: Option<String>,
    pub last_check_at: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct MakeProjectRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct PatchProjectRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub evaluation_wildcard: Option<String>,
}

pub async fn get(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(
        config,
        format!("projects/{}", organization),
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
    description: String,
    repository: String,
    evaluation_wildcard: String,
) -> Result<BaseResponse<String>, String> {
    let req = MakeProjectRequest {
        name,
        display_name,
        description,
        repository,
        evaluation_wildcard,
    };

    let res = get_client(
        config,
        format!("projects/{}", organization),
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

pub async fn get_project(
    config: RequestConfig,
    organization: String,
    projekt: String,
) -> Result<BaseResponse<ProjectResponse>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}", organization, projekt),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn patch_project(
    config: RequestConfig,
    organization: String,
    project: String,
    name: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    repository: Option<String>,
    evaluation_wildcard: Option<String>,
) -> Result<BaseResponse<String>, String> {
    let req = PatchProjectRequest {
        name,
        display_name,
        description,
        repository,
        evaluation_wildcard,
    };

    let res = get_client(
        config,
        format!("projects/{}/{}", organization, project),
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

pub async fn delete_project(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}", organization, project),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_project_active(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}/active", organization, project),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn delete_project_active(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}/active", organization, project),
        RequestType::DELETE,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_project_check_repository(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}/check-repository", organization, project),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_project_evaluate(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}/evaluate", organization, project),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
