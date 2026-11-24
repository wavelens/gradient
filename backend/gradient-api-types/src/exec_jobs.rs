/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use chrono::NaiveDateTime;

use serde::{Deserialize, Serialize};
use std::fmt;

#[repr(i16)]
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize, utoipa::ToSchema,
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(
        sea_orm::DeriveActiveEnum,
        sea_orm::EnumIter,
        num_enum::IntoPrimitive,
        num_enum::TryFromPrimitive
    )
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "snake_case")]
pub enum ExecJobStatus {
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Queued = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Starting = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    Running = 2,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Completed = 3,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    Failed = 4,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 5)
    )]
    Aborted = 5,
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 6)
    )]
    Created = 6,
}

impl ExecJobStatus {
    pub const OPEN: [Self; 4] = [Self::Created, Self::Queued, Self::Starting, Self::Running];

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Aborted)
    }
}

#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(
        sea_orm::DeriveActiveEnum,
        sea_orm::EnumIter,
        num_enum::IntoPrimitive,
        num_enum::TryFromPrimitive
    )
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "snake_case")]
pub enum AttemptEndReason {
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Completed = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Failed = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    Timeout = 2,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Lost = 3,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    Aborted = 4,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema)]
pub struct DeviceRequest {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceRequestError {
    #[error("device name is empty")]
    Empty,
    #[error("device name has an empty part before or after a colon")]
    EmptyPart,
    #[error("device count \"{0}\" is out of range")]
    BadCount(String),
    #[error("device count must be at least 1")]
    ZeroCount,
}

impl DeviceRequest {
    pub fn parse(s: &str) -> Result<DeviceRequest, DeviceRequestError> {
        let (name, count) = match s.rsplit_once(':') {
            Some((name, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                let count = tail
                    .parse::<u32>()
                    .map_err(|_| DeviceRequestError::BadCount(tail.to_owned()))?;
                (name, count)
            }
            _ => (s, 1),
        };
        if name.is_empty() {
            return Err(DeviceRequestError::Empty);
        }
        if name.split(':').any(str::is_empty) {
            return Err(DeviceRequestError::EmptyPart);
        }
        if count == 0 {
            return Err(DeviceRequestError::ZeroCount);
        }
        Ok(DeviceRequest {
            name: name.to_owned(),
            count,
        })
    }
}

impl fmt::Display for DeviceRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.count {
            1 => f.write_str(&self.name),
            count => write!(f, "{}:{count}", self.name),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, utoipa::ToSchema)]
pub struct GuestSize {
    pub cpus: u32,
    pub memory_mb: u64,
    pub disk_mb: u64,
}

impl GuestSize {
    pub fn default_request() -> GuestSize {
        GuestSize {
            cpus: 2,
            memory_mb: 500,
            disk_mb: 2048,
        }
    }

    pub fn is_unset(&self) -> bool {
        *self == GuestSize::default()
    }

    pub fn work_disk_size_mb_for_requested_disk_size_mb(disk_mb: u64) -> u64 {
        match disk_mb {
            0 => GuestSize::default_request().disk_mb,
            requested => requested,
        }
    }

    pub fn warm_guest_takes(&self, job: GuestSize) -> bool {
        self.cpus == job.cpus && self.memory_mb == job.memory_mb
    }

    pub fn warm(&self) -> GuestSize {
        GuestSize {
            disk_mb: 0,
            ..*self
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema)]
pub struct DeviceClass {
    pub name: String,
    pub total: u32,
    pub free: u32,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq, utoipa::ToSchema)]
pub struct ExecJobLog {
    pub offset: usize,
    pub log: String,
    pub total_bytes: usize,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecUsageChartPoint {
    pub elapsed_ms: u64,
    pub cpu_avg: f32,
    pub cpu_max: f32,
    pub ram_avg_bytes: u64,
    pub ram_max_bytes: u64,
    pub disk_max_bytes: u64,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobMetricsResponse {
    pub attempt: u32,
    pub attempt_open: bool,
    pub requested_cpus: u32,
    pub requested_memory_bytes: u64,
    pub requested_disk_bytes: u64,
    pub usable_memory_bytes: Option<u64>,
    pub usable_disk_bytes: Option<u64>,
    pub cpu_avg: f32,
    pub cpu_max: f32,
    pub ram_max_bytes: u64,
    pub disk_max_bytes: u64,
    pub points: Vec<ExecUsageChartPoint>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobOutputView {
    pub path: String,
    pub size: i64,
    pub content_type: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobPullRequestView {
    pub url: String,
    pub branch: String,
    pub number: i64,
    pub git_host_connection: GitHostConnectionId,
    pub created_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobAttemptView {
    pub number: i32,
    pub worker: String,
    pub started_at: NaiveDateTime,
    pub program_started_at: Option<NaiveDateTime>,
    pub ended_at: Option<NaiveDateTime>,
    pub end_reason: Option<AttemptEndReason>,
    pub message: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobNode {
    pub node: u32,
    pub id: ExecJobId,
    pub status: ExecJobStatus,
    pub worker: Option<String>,
    pub worker_name: Option<String>,
    pub guest_address: Option<String>,
    pub master_port: Option<u16>,
    pub attempts: Vec<ExecJobAttemptView>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobPhaseView {
    pub name: String,
    pub attempt: i32,
    pub created_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobTokenView {
    pub attempt: i32,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub revoked: bool,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct StartedEvaluationView {
    pub evaluation: EvaluationId,
    pub task: String,
    pub status: EvaluationStatus,
    pub created_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobResponse {
    pub id: ExecJobId,
    pub status: ExecJobStatus,
    pub entry_point: String,
    pub evaluation: EvaluationId,
    pub task: String,
    pub project: String,
    pub nodes: u32,
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub guest: GuestSize,
    pub architecture: String,
    pub mounts: Vec<String>,
    pub secrets: Vec<String>,
    pub network: bool,
    pub ssh: bool,
    pub ports: Vec<u16>,
    pub image: Option<String>,
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub retries: u32,
    pub waiting_reason: Option<WaitingReason>,
    pub message: Option<String>,
    pub retries_left: i32,
    pub autostart: bool,
    pub keep_vm_running: bool,
    pub program_exit_code: Option<i32>,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub worker: Option<String>,
    pub worker_name: Option<String>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub cluster_job: Option<ClusterJobId>,
    pub attempts: Vec<ExecJobAttemptView>,
    pub node_jobs: Vec<ExecJobNode>,
    pub phases: Vec<ExecJobPhaseView>,
    pub timeline: Vec<JobPhaseView>,
    pub tokens: Vec<ExecJobTokenView>,
    pub started_evaluations: Vec<StartedEvaluationView>,
    pub outputs_size: i64,
    pub ssh_command: Option<String>,
    pub forward_commands: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobSummary {
    pub id: ExecJobId,
    pub entry_point: String,
    pub status: ExecJobStatus,
    pub waiting_reason: Option<WaitingReason>,
    pub message: Option<String>,
    pub nodes: u32,
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub autostart: bool,
    pub keep_vm_running: bool,
    pub program_exit_code: Option<i32>,
    pub started_at: Option<NaiveDateTime>,
    pub last_phase: Option<String>,
    pub end_reason: Option<AttemptEndReason>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub cluster_job: Option<ClusterJobId>,
}

impl ExecJobLog {
    pub fn after(mut full: String, offset: Option<usize>) -> Self {
        let total_bytes = full.len();
        let offset = offset
            .filter(|&offset| full.is_char_boundary(offset))
            .unwrap_or(0);
        let log = full.split_off(offset);
        Self {
            offset,
            log,
            total_bytes,
        }
    }
}
