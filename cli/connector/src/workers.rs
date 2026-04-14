/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct RegisterWorkerRequest {
    pub worker_id: String,
    pub url: Option<String>,
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RegisterWorkerResponse {
    pub peer_id: String,
    /// Absent when the token was supplied in the request.
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct WorkerLiveInfo {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct OrgWorkerEntry {
    pub worker_id: String,
    pub registered_at: String,
    pub url: Option<String>,
    pub live: Option<WorkerLiveInfo>,
}

pub async fn post_org_worker(
    config: RequestConfig,
    organization: String,
    worker_id: String,
    url: Option<String>,
    token: Option<String>,
) -> Result<BaseResponse<RegisterWorkerResponse>, String> {
    let req = RegisterWorkerRequest {
        worker_id,
        url,
        token,
    };

    let res = get_client(
        config,
        format!("orgs/{}/workers", organization),
        RequestType::POST,
        true,
    )?
    .json(&req)
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn get_org_workers(
    config: RequestConfig,
    organization: String,
) -> Result<BaseResponse<Vec<OrgWorkerEntry>>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/workers", organization),
        RequestType::GET,
        true,
    )?
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}

pub async fn delete_org_worker(
    config: RequestConfig,
    organization: String,
    worker_id: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("orgs/{}/workers/{}", organization, worker_id),
        RequestType::DELETE,
        true,
    )?
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
