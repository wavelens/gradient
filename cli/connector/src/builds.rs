/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use futures::stream::StreamExt;
use reqwest::multipart::{Form, Part};
use reqwest_streams::JsonStreamResponse;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildResponse {
    pub id: String,
    pub evaluation: String,
    pub status: String,
    pub derivation_path: String,
    pub architecture: String,
    pub server: Option<String>,
    pub log: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildProduct {
    pub file_type: String,
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DirectBuildInfo {
    pub id: String,
    pub derivation: String,
    pub created_at: String,
    pub evaluation_id: String,
    pub status: String,
}

pub async fn get_build(
    config: RequestConfig,
    build_id: String,
) -> Result<BaseResponse<BuildResponse>, String> {
    let res = get_client(
        config,
        format!("builds/{}", build_id),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_build(config: RequestConfig, build_id: String) -> Result<(), String> {
    let mut stream = get_client(
        config,
        format!("builds/{}", build_id),
        RequestType::POST,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap()
    .json_nl_stream::<String>(1024000);

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => print!("{}", chunk),
            Err(e) => return Err(e.to_string()),
        }
    }

    Ok(())
}

pub async fn post_direct_build(
    config: RequestConfig,
    organization: String,
    derivation: String,
    files: HashMap<String, Vec<u8>>,
) -> Result<BaseResponse<String>, String> {
    let mut form = Form::new()
        .text("organization", organization)
        .text("derivation", derivation);

    // Add files to form
    for (filename, data) in files {
        let part = Part::bytes(data).file_name(filename.clone());
        form = form.part(format!("file:{}", filename), part);
    }

    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/builds", config.server_url);

    let res = client
        .post(&url)
        .header(
            "Authorization",
            format!("Bearer {}", config.token.unwrap_or_default()),
        )
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    Ok(parse_response(res).await)
}

pub async fn get_build_downloads(
    config: RequestConfig,
    build_id: String,
) -> Result<BaseResponse<Vec<BuildProduct>>, String> {
    let res = get_client(
        config,
        format!("builds/{}/downloads", build_id),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn download_build_file(
    config: RequestConfig,
    build_id: String,
    filename: String,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/v1/builds/{}/download/{}",
        config.server_url, build_id, filename
    );

    let res = client
        .get(&url)
        .header(
            "Authorization",
            format!("Bearer {}", config.token.unwrap_or_default()),
        )
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !res.status().is_success() {
        return Err(format!("Download failed with status: {}", res.status()));
    }

    res.bytes()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))
        .map(|b| b.to_vec())
}

pub async fn get_recent_direct_builds(
    config: RequestConfig,
) -> Result<BaseResponse<Vec<DirectBuildInfo>>, String> {
    let res = get_client(
        config,
        "builds/direct/recent".to_string(),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_evaluation_builds(
    config: RequestConfig,
    evaluation_id: String,
) -> Result<BaseResponse<Vec<crate::ListItem>>, String> {
    let res = get_client(
        config,
        format!("evals/{}/builds", evaluation_id),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
