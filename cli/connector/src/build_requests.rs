/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ManifestFile {
    pub path: String,
    pub hash: String,
    pub size: i64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ManifestRequest {
    pub organization: String,
    pub files: Vec<ManifestFile>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ManifestResponse {
    pub session: String,
    pub missing: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BlobsResponse {
    pub uploaded: usize,
    pub remaining: usize,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct DispatchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DispatchResponse {
    pub evaluation: String,
    pub project: String,
    pub commit: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ArtefactTree {
    pub evaluation: String,
    pub created_at: String,
    pub entry_points: Vec<EntryPointArtefacts>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointArtefacts {
    pub attr: String,
    pub derivation: String,
    pub build_id: String,
    pub outputs: Vec<OutputArtefacts>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct OutputArtefacts {
    pub name: String,
    pub store_path: String,
    pub products: Vec<ProductArtefact>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProductArtefact {
    pub id: String,
    #[serde(rename = "type")]
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<i64>,
}

pub async fn post_manifest(
    config: RequestConfig,
    body: ManifestRequest,
) -> Result<BaseResponse<ManifestResponse>, String> {
    let res = get_client(
        config,
        "build-requests/manifest".to_string(),
        RequestType::POST,
        true,
    )
    .unwrap()
    .json(&body)
    .send()
    .await
    .map_err(|e| e.to_string())?;

    Ok(parse_response(res).await)
}

pub async fn upload_blobs<I>(
    config: RequestConfig,
    session: String,
    blobs: I,
) -> Result<BaseResponse<BlobsResponse>, String>
where
    I: IntoIterator<Item = (String, Vec<u8>)>,
{
    let mut form = Form::new();
    for (hash, bytes) in blobs {
        let part = Part::bytes(bytes).file_name(hash.clone());
        form = form.part(hash, part);
    }

    let url = format!(
        "{}/api/v1/build-requests/{}/blobs",
        config.server_url, session
    );

    let res = http_client()
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

pub async fn dispatch_build_request(
    config: RequestConfig,
    session: String,
    body: DispatchRequest,
) -> Result<BaseResponse<DispatchResponse>, String> {
    let res = get_client(
        config,
        format!("build-requests/{}/dispatch", session),
        RequestType::POST,
        true,
    )
    .unwrap()
    .json(&body)
    .send()
    .await
    .map_err(|e| e.to_string())?;

    Ok(parse_response(res).await)
}

pub async fn get_eval_artefacts(
    config: RequestConfig,
    evaluation_id: String,
) -> Result<BaseResponse<ArtefactTree>, String> {
    let res = get_client(
        config,
        format!("evals/{}/artefacts", evaluation_id),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .map_err(|e| e.to_string())?;

    Ok(parse_response(res).await)
}
