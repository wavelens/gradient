/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use chrono::NaiveDateTime;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ManifestResult {
    pub id: i64,
    pub slug: String,
    pub html_url: String,
    pub pem: String,
    pub webhook_secret: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SetDrainingRequest {
    pub enabled: bool,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct DrainingResponse {
    pub draining: bool,
}

#[derive(Deserialize, Serialize, Default, Debug, utoipa::ToSchema)]
pub struct GithubAppManifestRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct GithubAppManifestResponse {
    pub manifest: serde_json::Value,
    pub post_url: String,
    pub state: String,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct StartDeepGcResponse {
    pub task_id: AdminTaskId,
    pub status: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct CacheStorageSummary {
    pub id: CacheId,
    pub name: String,
    pub display_name: String,
    pub max_storage_gb: i32,
    pub packages: i64,
    pub bytes: i64,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProjectStoredBytes {
    pub project: BoardProject,
    pub packages: i64,
    pub bytes: i64,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TaskStoredBytes {
    pub id: TaskId,
    pub name: String,
    pub display_name: String,
    pub project: BoardProject,
    pub keep_evaluations: i32,
    pub evaluations: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_evaluation_at: Option<NaiveDateTime>,
    pub packages: i64,
    pub bytes: i64,
}

#[derive(Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(default)]
pub struct DeepGarbageCollectionStats {
    #[serde(skip_deserializing)]
    pub finished_at: Option<NaiveDateTime>,
    pub nars_scanned: u64,
    pub orphan_nars_removed: u64,
    pub blobs_scanned: u64,
    pub orphan_blobs_removed: u64,
    pub logs_scanned: u64,
    pub orphan_logs_removed: u64,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct AdminStorageResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_free_bytes: Option<u64>,
    pub caches: Vec<CacheStorageSummary>,
    pub projects: Vec<ProjectStoredBytes>,
    pub tasks: Vec<TaskStoredBytes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_deep_gc: Option<DeepGarbageCollectionStats>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct AdminTaskDto {
    pub id: AdminTaskId,
    pub kind: String,
    pub status: String,
    pub created_at: chrono::NaiveDateTime,
    pub started_at: Option<chrono::NaiveDateTime>,
    pub finished_at: Option<chrono::NaiveDateTime>,
    pub progress: Option<serde_json::Value>,
    pub checkpoint: Option<String>,
    pub error: Option<String>,
    pub created_by: Option<UserId>,
}

#[derive(Deserialize, Serialize, Clone, utoipa::ToSchema)]
pub struct WorkerOwningTeam {
    pub id: TeamId,
    pub name: String,
    pub display_name: String,
    pub active: bool,
    pub managed: bool,
}

#[derive(Deserialize, Serialize, Clone, utoipa::ToSchema)]
pub struct WorkerProjectRegistration {
    pub project: BoardProject,
    pub display_name: String,
    pub active: bool,
    pub managed: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct AdminWorkerListEntry {
    pub worker_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<WorkerOwningTeam>,
    pub registrations: Vec<WorkerProjectRegistration>,
    pub projects: Vec<BoardProject>,
    #[serde(flatten)]
    pub connection: WorkerConnection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerInfo>,
    pub requested_by_admin: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct UpdateWorkerRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draining: Option<bool>,
}
