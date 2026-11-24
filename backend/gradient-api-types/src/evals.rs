/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;

use crate::*;
use serde::{Deserialize, Serialize};

#[repr(i32)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
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
    sea_orm(rs_type = "i32", db_type = "Integer")
)]
pub enum EvaluationStatus {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Queued = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    EvaluatingFlake = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    EvaluatingDerivation = 2,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Building = 3,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    Waiting = 4,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 5)
    )]
    Completed = 5,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 6)
    )]
    Failed = 6,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 7)
    )]
    Aborted = 7,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 8)
    )]
    Fetching = 8,
}

impl EvaluationStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Fetching
                | Self::EvaluatingFlake
                | Self::EvaluatingDerivation
                | Self::Building
                | Self::Waiting
        )
    }

    pub const ACTIVE: [Self; 6] = [
        Self::Queued,
        Self::Fetching,
        Self::EvaluatingFlake,
        Self::EvaluatingDerivation,
        Self::Building,
        Self::Waiting,
    ];

    pub const DOING_WORK_STATUSES: [Self; 4] = [
        Self::Fetching,
        Self::EvaluatingFlake,
        Self::EvaluatingDerivation,
        Self::Building,
    ];

    pub const TERMINAL: [Self; 3] = [Self::Completed, Self::Failed, Self::Aborted];

    pub const AVERAGE_RUN_TIME_EVALUATION_STATUSES: [Self; 2] = [Self::Completed, Self::Failed];

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Aborted)
    }

    pub const fn sources(self) -> &'static [Self] {
        match self {
            Self::Queued => &[
                Self::Waiting,
                Self::Fetching,
                Self::EvaluatingFlake,
                Self::EvaluatingDerivation,
            ],
            Self::Fetching => &[Self::Queued],
            Self::EvaluatingFlake => &[Self::Queued, Self::Fetching],
            Self::EvaluatingDerivation => &[Self::EvaluatingFlake],
            Self::Building => &[
                Self::EvaluatingFlake,
                Self::EvaluatingDerivation,
                Self::Waiting,
            ],
            Self::Waiting => &[
                Self::Building,
                Self::Queued,
                Self::Fetching,
                Self::EvaluatingFlake,
                Self::EvaluatingDerivation,
            ],
            Self::Completed | Self::Failed | Self::Aborted => &Self::ACTIVE,
        }
    }

    pub const fn report(self) -> Transition<Self> {
        Transition {
            from: self.sources(),
            to: self,
        }
    }

    pub const EVALUATING: [Self; 2] = [Self::EvaluatingFlake, Self::EvaluatingDerivation];

    pub const REOPENABLE_FOR_EXEC_JOB: [Self; 3] = [Self::Failed, Self::Aborted, Self::Completed];

    pub const REOPEN_FOR_EXEC_JOB: Transition<Self> = Transition {
        from: &Self::REOPENABLE_FOR_EXEC_JOB,
        to: Self::Building,
    };

    pub const PARK: Transition<Self> = Transition {
        from: &[Self::Queued],
        to: Self::Waiting,
    };

    pub const UNPARK: Transition<Self> = Transition {
        from: &[Self::Waiting],
        to: Self::Queued,
    };

    pub const ABORT: Transition<Self> = Transition {
        from: &Self::ACTIVE,
        to: Self::Aborted,
    };

    pub const WAIT_FOR_DRAINING: Transition<Self> = Transition {
        from: &[
            Self::Queued,
            Self::Fetching,
            Self::EvaluatingFlake,
            Self::EvaluatingDerivation,
            Self::Building,
        ],
        to: Self::Waiting,
    };

    pub const REQUEUE_INTERRUPTED: Transition<Self> = Transition {
        from: &[
            Self::Fetching,
            Self::EvaluatingFlake,
            Self::EvaluatingDerivation,
        ],
        to: Self::Queued,
    };
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
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
    sea_orm(rs_type = "i32", db_type = "Integer")
)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    User = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Superseded = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    NoConnectedWorkerCanBuildRequiredSystems = 2,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(sea_orm::FromJsonQueryResult)
)]
pub struct RunOptions {
    #[serde(default)]
    pub interactive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
}

impl RunOptions {
    pub fn exec_block(&self) -> serde_json::Value {
        let mut block = serde_json::Map::new();
        let mut set = |key: &str, value: Option<serde_json::Value>| {
            if let Some(value) = value {
                block.insert(key.to_owned(), value);
            }
        };
        set("cpus", self.cpus.map(Into::into));
        set("memoryMb", self.memory_mb.map(Into::into));
        set("network", self.network.map(Into::into));
        set("labels", self.labels.clone().map(Into::into));
        set("timeout", self.timeout_secs.map(Into::into));
        set("maxSilent", self.timeout_secs.map(Into::into));
        if !self.ports.is_empty() {
            set("ports", Some(self.ports.clone().into()));
            set("ssh", Some(true.into()));
        }
        serde_json::Value::Object(block)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(sea_orm::DeriveActiveEnum, sea_orm::EnumIter)
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i32", db_type = "Integer")
)]
pub enum MessageLevel {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Error,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Warning,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    Notice,
}

impl From<EvaluationStatus> for i16 {
    fn from(status: EvaluationStatus) -> Self {
        status as Self
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ArtefactTree {
    pub evaluation: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<EntryPointArtefacts>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointArtefacts {
    pub attr: String,
    pub derivation: String,
    pub build_id: BuildJobId,
    pub outputs: Vec<OutputArtefacts>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct OutputArtefacts {
    pub name: String,
    pub store_path: String,
    pub products: Vec<ProductArtefact>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ProductArtefact {
    pub id: BuildProductId,
    #[serde(rename = "type")]
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<i64>,
}

#[derive(Deserialize, Serialize, Debug, Default, PartialEq, Eq, utoipa::ToSchema)]
pub struct PackageCounts {
    pub total: usize,
    pub built: usize,
    pub failed: usize,
    pub blocked: usize,
    pub unfinished: usize,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct EvaluationFailureSummary {
    pub compared_with: Option<ComparedEvaluation>,
    pub packages: PackageCounts,
    pub failures: Vec<FailedBuildSummary>,
    pub failures_total: usize,
    pub failed_attributes: Vec<FailedAttributeReport>,
    pub fixed: Vec<String>,
    pub fixed_total: usize,
    pub unfinished_attributes: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ComparedEvaluation {
    pub id: EvaluationId,
    pub commit: String,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct FailedBuildSummary {
    pub build_id: BuildJobId,
    pub name: String,
    pub derivation_path: String,
    #[schema(value_type = String)]
    pub architecture: Architecture,
    pub status: BuildStatus,
    pub attributes: Vec<String>,
    pub blocked: Vec<String>,
    pub blocked_total: usize,
    pub newly_failing: bool,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct FailedAttributeReport {
    pub attr: String,
    pub message: String,
    pub newly_failing: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HistoryBar {
    pub id: EvaluationId,
    pub status: EvaluationStatus,
    pub duration_ms: Option<i64>,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EvaluationAction {
    Abort,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeEvaluationRequest {
    pub method: EvaluationAction,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct BuildItem {
    pub id: BuildJobId,
    pub name: String,
    pub status: String,
    pub has_artefacts: bool,
    pub updated_at: chrono::NaiveDateTime,
    pub build_time_ms: Option<i64>,
    pub build_started_at: Option<chrono::NaiveDateTime>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub depth: u32,
    pub prioritized: bool,
    pub ifd: bool,
    pub continued_in_evaluation: Option<EvaluationId>,
    pub entry_points: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct PaginatedBuilds {
    pub builds: Vec<BuildItem>,
    pub total: usize,
    pub active_count: usize,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct EvaluationResponse {
    pub id: EvaluationId,
    pub task: Option<TaskId>,
    pub project_name: String,
    pub task_name: Option<String>,
    pub task_display_name: Option<String>,
    pub repository: String,
    pub commit: String,
    pub wildcard: String,
    pub only_evaluate: bool,
    pub status: EvaluationStatus,
    pub previous: Option<EvaluationId>,
    pub next: Option<EvaluationId>,
    pub created_at: chrono::NaiveDateTime,
    pub started_at: Option<chrono::NaiveDateTime>,
    pub finished_at: Option<chrono::NaiveDateTime>,
    pub updated_at: chrono::NaiveDateTime,
    pub error_count: u64,
    pub warning_count: u64,
    pub error: Option<String>,
    pub entry_points: Vec<EntryPointBrief>,
    pub prioritized: bool,
    pub trigger: Option<EvaluationTriggerSummary>,
    pub triggered_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<EvaluationProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_thunks: Option<u64>,
    #[serde(flatten)]
    pub history: EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration,
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, utoipa::ToSchema)]
pub struct EvaluationPhaseStartTimestamps {
    pub fetch: Option<chrono::NaiveDateTime>,
    pub eval_flake: Option<chrono::NaiveDateTime>,
    pub eval_derivation: Option<chrono::NaiveDateTime>,
    pub building: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, utoipa::ToSchema)]
pub struct EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration {
    pub abort_reason: Option<AbortReason>,
    pub aborted_by: Option<String>,
    pub superseded_by: Option<EvaluationId>,
    pub retry_of: Option<EvaluationId>,
    pub phase_started_at: EvaluationPhaseStartTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_duration_ms: Option<u64>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct EntryPointBrief {
    pub id: EntryPointId,
    pub eval: String,
    pub build_status: BuildStatus,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct EvaluationMessageResponse {
    pub id: EvaluationMessageId,
    pub level: MessageLevel,
    pub message: String,
    pub source: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<EntryPointId>,
}
