/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct BlobsResponse {
    pub uploaded: usize,
    pub remaining: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, utoipa::ToSchema)]
pub struct InputOverrideBody {
    pub input_name: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Default, utoipa::ToSchema)]
pub struct DispatchRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default)]
    pub input_overrides: Vec<InputOverrideBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunOptions>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct DispatchResponse {
    pub evaluation: EvaluationId,
    pub task: TaskId,
    pub commit: CommitId,
    pub cache: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ManifestRequest {
    pub project: String,
    pub files: Vec<ManifestEntry>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ManifestResponse {
    pub session: UploadSessionId,
    pub missing: Vec<String>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SourceFinalize {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub input_overrides: Vec<InputOverrideBody>,
    #[serde(default)]
    pub run: Option<RunOptions>,
}

#[derive(Serialize, Deserialize, Debug, Clone, utoipa::ToSchema)]
pub struct ManifestEntry {
    pub path: String,
    pub hash: String,
    pub size: i64,
}
