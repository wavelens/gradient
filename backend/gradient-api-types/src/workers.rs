/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use std::collections::HashSet;
use utoipa::ToSchema;

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionDirection {
    Outbound,
    Inbound,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ConnectionFailure {
    pub reason: String,
    pub at: NaiveDateTime,
    pub direction: ConnectionDirection,
    pub before_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct ExecOffer {
    pub labels: Vec<String>,
    pub devices: Vec<DeviceClass>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, utoipa::ToSchema)]
pub struct GradientCapabilities {
    pub core: bool,
    pub federate: bool,
    pub eval: bool,
    pub build: bool,
    pub cache: bool,
    pub network: bool,
    pub secrets: bool,
    pub exec: bool,
}

impl std::ops::BitOrAssign for GradientCapabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        self.core |= rhs.core;
        self.federate |= rhs.federate;
        self.eval |= rhs.eval;
        self.build |= rhs.build;
        self.cache |= rhs.cache;
        self.network |= rhs.network;
        self.secrets |= rhs.secrets;
        self.exec |= rhs.exec;
    }
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct RegisterWorkerRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub display_name: String,
    #[serde(default = "default_true")]
    pub enable_network: bool,
    #[serde(default = "default_true")]
    pub enable_secrets: bool,
    #[serde(default)]
    pub enable_exec: bool,
    #[serde(default = "default_true")]
    pub enable_eval: bool,
    #[serde(default = "default_true")]
    pub enable_build: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProjectWorkerEntry {
    pub worker_id: String,
    pub display_name: String,
    pub registered_at: NaiveDateTime,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_by: Option<UserId>,
    pub enable_network: bool,
    pub enable_secrets: bool,
    pub enable_exec: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_display_name: Option<String>,
    pub gradient_ci: bool,
    #[serde(flatten)]
    pub connection: WorkerConnection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerLiveInfo>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkerConnection {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ConnectionFailure>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema, Default, PartialEq)]
pub struct PatchWorkerRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_network: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_secrets: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_exec: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_eval: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_build: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draining: Option<bool>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkerLiveInfo {
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_total_mb: Option<u64>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkerSamplePoint {
    pub at: NaiveDateTime,
    pub cpu_usage_pct: Option<f32>,
    pub ram_free_mb: Option<i64>,
    pub ram_total_mb: Option<i64>,
    pub disk_speed_mbps: Option<f32>,
    pub upload_speed_mbps: Option<f32>,
    pub download_speed_mbps: Option<f32>,
    pub assigned_jobs: i32,
    pub max_concurrent_builds: i32,
    pub state: i16,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkerConnectionEntry {
    pub connected_at: NaiveDateTime,
    pub disconnected_at: Option<NaiveDateTime>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkerMetricsResponse {
    pub worker_id: String,
    pub display_name: String,
    pub samples: Vec<WorkerSamplePoint>,
    pub connections: Vec<WorkerConnectionEntry>,
    pub jobs_dispatched: u64,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkerTestResponse {
    pub ok: bool,
    pub connected: bool,
    pub authorized_for_project: bool,
    pub message: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WorkerRegistrationResponse {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Project ID or team ID that the worker names in its peers file. The peers line is `<peer_id>:<token>`.
    pub peer_id: String,
    /// One-time token for the worker. The server can save only the hash and can't show the token again.
    pub token: String,
    pub nixos_configuration: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkerInfo {
    pub id: String,
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
    pub authorized_peers: Option<HashSet<ProjectId>>,
    pub exec: Option<ExecOffer>,
}
