/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use futures::stream::StreamExt;
use reqwest_streams::JsonStreamResponse;
use serde::{Deserialize, Serialize};

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
