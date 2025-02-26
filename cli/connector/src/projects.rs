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

pub async fn get(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(
        config,
        format!("projects/{}", organization),
        RequestType::Get,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn post(
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
        RequestType::Post,
        true,
    )
    .unwrap()
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn get_project(
    config: RequestConfig,
    organization: String,
    projekt: String,
) -> Result<BaseResponse<ProjectResponse>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}", organization, projekt),
        RequestType::Get,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn delete_project(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}", organization, project),
        RequestType::Delete,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn post_project_check_repository(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}/check-repository", organization, project),
        RequestType::Post,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}

pub async fn post_project_evaluate(
    config: RequestConfig,
    organization: String,
    project: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("projects/{}/{}/evaluate", organization, project),
        RequestType::Post,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await.unwrap())
}
