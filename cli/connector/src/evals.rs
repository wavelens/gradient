/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use futures::stream::StreamExt;
use reqwest_streams::JsonStreamResponse;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildItem {
    pub id: String,
    pub name: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EvaluationResponse {
    pub id: String,
    pub project: String,
    pub repository: String,
    pub commit: String,
    pub wildcard: String,
    pub status: String,
    pub previous: Option<String>,
    pub next: Option<String>,
    pub created_at: String,
    pub error: Option<String>,
}

pub async fn get_evaluation(
    config: RequestConfig,
    evaluation_id: String,
) -> Result<BaseResponse<EvaluationResponse>, String> {
    let res = get_client(
        config,
        format!("evals/{}", evaluation_id),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn post_evaluation(
    config: RequestConfig,
    evaluation_id: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("evals/{}", evaluation_id),
        RequestType::POST,
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
) -> Result<BaseResponse<Vec<BuildItem>>, String> {
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

pub async fn post_evaluation_builds(
    config: RequestConfig,
    evaluation_id: String,
) -> Result<(), String> {
    let mut stream = get_client(
        config,
        format!("evals/{}/builds", evaluation_id),
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
