/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use utoipa::ToSchema;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[repr(i16)]
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
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    HardAbort = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    SoftAbort = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    All = 2,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Skip = 3,
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    Auto = 4,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ActionResponse {
    pub id: TaskActionId,
    pub name: String,
    pub action_type: String,
    pub config: JsonValue,
    pub events: Vec<String>,
    pub active: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub last_delivery: Option<ActionDeliveryItem>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateActionResponse {
    pub action: ActionResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateActionRequest {
    pub name: String,
    pub config: ActionConfig,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct UpdateActionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ActionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct FlakeInputOverrideOut {
    pub id: FlakeInputOverrideId,
    pub task: TaskId,
    pub input_name: String,
    pub url: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateFlakeInputOverrideRequest {
    pub input_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct UpdateFlakeInputOverrideRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::deserialize_optional_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub url: Option<Option<String>>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeTaskRequest {
    pub name: TaskName,
    pub display_name: TaskDisplayName,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<ConcurrencyPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for_workers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_failed_builds: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_node_evaluation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restrict_evaluation_fetches: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_allowed_uri_prefixes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggers: Option<Vec<TriggerConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_host_connection: Option<GitHostConnectionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_status: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct DuplicateTaskRequest {
    pub name: TaskName,
    pub display_name: TaskDisplayName,
    pub branch: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema, Default)]
pub struct PatchTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<TaskName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wildcard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_evaluations: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<ConcurrencyPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_workers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_failed_builds: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_node_evaluation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only_evaluate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restrict_evaluation_fetches: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_allowed_uri_prefixes: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TransferOwnershipRequest {
    pub project: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TaskMetricPoint {
    pub evaluation_id: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub build_time_total_ms: i64,
    pub eval_time_ms: Option<i64>,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub runtime_closure_size_bytes: Option<i64>,
    pub dependencies_count: i64,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TaskMetricsResponse {
    pub keep_evaluations: i32,
    pub points: Vec<TaskMetricPoint>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointMetricPoint {
    pub evaluation_id: EvaluationId,
    pub build_id: BuildJobId,
    pub created_at: chrono::NaiveDateTime,
    pub build_status: BuildStatus,
    pub build_time_ms: Option<i64>,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub runtime_closure_size_bytes: Option<i64>,
    pub dependencies_count: i64,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointMetricsResponse {
    pub eval: String,
    pub keep_evaluations: i32,
    pub points: Vec<EntryPointMetricPoint>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TaskResponse {
    pub id: TaskId,
    pub project: ProjectId,
    pub name: String,
    pub active: bool,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    pub last_evaluation: Option<EvaluationId>,
    pub last_evaluation_status: Option<EvaluationStatus>,
    pub recent_evaluations: Vec<HistoryBar>,
    pub force_evaluation: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
    pub keep_evaluations: i32,
    pub concurrency: ConcurrencyPolicy,
    pub wait_for_workers: bool,
    pub retry_failed_builds: bool,
    pub multi_node_evaluation: bool,
    pub exec_enabled: bool,
    pub only_evaluate: bool,
    pub restrict_evaluation_fetches: bool,
    pub extra_allowed_uri_prefixes: Vec<String>,
    pub repository_fetch: Option<RepositoryFetchStatus>,
    pub branches: Vec<Option<String>>,
    pub next_polling_trigger_check_at: Option<chrono::NaiveDateTime>,
    pub can_edit: bool,
    pub can_trigger: bool,
    pub permissions: Vec<Permission>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct RepositoryFetchStatus {
    pub checked_at: chrono::NaiveDateTime,
    pub ok_at: Option<chrono::NaiveDateTime>,
    pub failed: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointSummary {
    pub id: EntryPointId,
    pub build_id: BuildJobId,
    pub derivation_path: String,
    pub eval: String,
    pub build_status: BuildStatus,
    pub has_artefacts: bool,
    pub outputs: std::collections::BTreeMap<String, String>,
    #[schema(value_type = String)]
    pub architecture: Architecture,
    pub build_time_ms: Option<i64>,
    pub build_started_at: Option<chrono::NaiveDateTime>,
    pub deps: BuildStatusCounts,
    pub deps_total: i64,
    pub prioritized: bool,
    pub ifd: bool,
    pub created_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, utoipa::ToSchema)]
pub struct FailedAttributeSummary {
    pub eval: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Default, utoipa::ToSchema)]
pub struct PaginatedEntryPoints {
    pub entry_points: Vec<EntryPointSummary>,
    pub total: u64,
    pub failed_attributes: Vec<FailedAttributeSummary>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EvaluationTriggerSummary {
    pub id: TaskTriggerId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct DeletedResponse {
    pub deleted: bool,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EvaluationSummary {
    pub id: EvaluationId,
    pub commit: String,
    pub commit_message: Option<String>,
    pub status: EvaluationStatus,
    pub wildcard: String,
    pub trigger: Option<EvaluationTriggerSummary>,
    pub triggered_by: Option<String>,
    pub pr_number: Option<u64>,
    pub total_builds: i64,
    pub builds: BuildStatusCounts,
    pub errors: i64,
    pub warnings: i64,
    pub dispatched_job: Option<DispatchedJobId>,
    pub cluster_job: Option<ClusterJobId>,
    pub prioritized: bool,
    pub only_evaluate: bool,
    pub created_at: chrono::NaiveDateTime,
    pub started_at: Option<chrono::NaiveDateTime>,
    pub finished_at: Option<chrono::NaiveDateTime>,
    pub updated_at: chrono::NaiveDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<EvaluationProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_thunks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    #[serde(flatten)]
    pub history: EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TaskDetailsResponse {
    pub id: TaskId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    pub active: bool,
    pub created_at: chrono::NaiveDateTime,
    pub keep_evaluations: i32,
    pub last_check_at: Option<chrono::NaiveDateTime>,
    pub repository_fetch: Option<RepositoryFetchStatus>,
    pub branches: Vec<Option<String>>,
    pub next_polling_trigger_check_at: Option<chrono::NaiveDateTime>,
    pub queue: QueueSummary,
    pub last_evaluations: Vec<EvaluationSummary>,
    pub can_edit: bool,
    pub can_trigger: bool,
    pub permissions: Vec<Permission>,
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, utoipa::ToSchema)]
pub struct QueueSummary {
    pub building: i64,
    pub queued: i64,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreatePullRequestRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub patch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct PullRequestResponse {
    pub url: String,
    pub number: i64,
    pub branch: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct TaskCommitStatusPostAttempt {
    pub id: TaskCommitStatusPostAttemptId,
    pub event: String,
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub delivered_at: chrono::NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct TaskRepositoryResponse {
    pub repository: String,
    pub uses_ssh: bool,
    pub deploy_key: Option<String>,
    pub fetch: Option<RepositoryFetchStatus>,
    pub git_host_connection: Option<GitHostConnectionSummary>,
    pub report_status: bool,
    pub chain: TaskGitHostChain,
    #[serde(default)]
    #[schema(required = false, default = json!([]))]
    pub recent_deliveries: Vec<IncomingWebhookDeliveryResponse>,
    pub status_posts: Vec<TaskCommitStatusPostAttempt>,
    pub available_connections: Vec<GitHostConnectionSummary>,
    pub can_edit: bool,
    pub can_manage_integrations: bool,
    #[serde(default)]
    #[schema(required = false, default = false)]
    pub can_create_connection: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskGitHostChainStepState {
    Ok,
    Attention,
    Failing,
    Off,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskGitHostChainStepAction {
    EditRepositoryUrl,
    SelectConnection,
    CreateConnection {
        git_host_type: Option<String>,
        base_url: String,
    },
    InstallGithubApp {
        account: String,
    },
    InstallWebhook,
    AddWebhookOnHost {
        webhook_url: String,
    },
    ChangeWebhookSecret,
    OpenConnectionSharedByTasks {
        task_count: u64,
    },
    AddTrigger {
        trigger_type: TriggerType,
    },
    TurnOnReportStatus,
    TestStatusReport,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
pub struct TaskGitHostChainStep {
    pub state: TaskGitHostChainStepState,
    pub detail: String,
    pub at: Option<chrono::NaiveDateTime>,
    pub action: Option<TaskGitHostChainStepAction>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
pub struct TaskGitHostChain {
    pub repository: TaskGitHostChainStep,
    pub connection: TaskGitHostChainStep,
    pub incoming_events: TaskGitHostChainStep,
    pub commit_status: TaskGitHostChainStep,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct TriggerOut {
    pub id: TaskTriggerId,
    pub task: TaskId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    pub config: serde_json::Value,
    pub active: bool,
    pub is_default: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
    pub has_secret: bool,
    pub post_request_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_secret: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, ToSchema)]
pub struct CreateTriggerRequest {
    pub config: TriggerConfig,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Deserialize, Serialize, Debug, ToSchema)]
pub struct UpdateTriggerRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<TriggerConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, utoipa::ToSchema)]
pub struct BuildStatusCounts {
    pub completed: i64,
    pub failed: i64,
    pub building: i64,
    pub queued: i64,
    pub substituted: i64,
    pub aborted: i64,
    pub dependency_failed: i64,
}

impl EvaluationTriggerSummary {
    pub fn new(id: TaskTriggerId, trigger_type: TriggerType) -> Self {
        Self { id, trigger_type }
    }
}

impl DeletedResponse {
    pub fn deleted() -> Self {
        Self { deleted: true }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BarSegment {
    Completed,
    Failed,
    Building,
    Queued,
    Substituted,
    Aborted,
    DependencyFailed,
}

/// `Skipped` is neither pending work nor a result. Drawing it would make a task read as unfinished
/// forever.
pub fn bar_segment(status: BuildStatus) -> Option<BarSegment> {
    use BuildStatus::*;
    Some(match status {
        Completed => BarSegment::Completed,
        FailedPermanent | FailedTimeout => BarSegment::Failed,
        Building => BarSegment::Building,
        Queued | Created | FailedTransient => BarSegment::Queued,
        Substituted => BarSegment::Substituted,
        Aborted => BarSegment::Aborted,
        DependencyFailed => BarSegment::DependencyFailed,
        Skipped => return None,
    })
}

impl BuildStatusCounts {
    pub fn add(&mut self, status: BuildStatus, n: i64) {
        match bar_segment(status) {
            Some(BarSegment::Completed) => self.completed += n,
            Some(BarSegment::Failed) => self.failed += n,
            Some(BarSegment::Building) => self.building += n,
            Some(BarSegment::Queued) => self.queued += n,
            Some(BarSegment::Substituted) => self.substituted += n,
            Some(BarSegment::Aborted) => self.aborted += n,
            Some(BarSegment::DependencyFailed) => self.dependency_failed += n,
            None => {}
        }
    }

    pub fn total(&self) -> i64 {
        self.completed
            + self.failed
            + self.building
            + self.queued
            + self.substituted
            + self.aborted
            + self.dependency_failed
    }
}

#[cfg(test)]
mod rollup_tests {
    use super::{BarSegment, BuildStatus, BuildStatusCounts, bar_segment};

    #[test]
    fn segment_mapping_matches_spec() {
        use BuildStatus::*;
        assert_eq!(bar_segment(Completed), Some(BarSegment::Completed));
        for s in [FailedPermanent, FailedTimeout] {
            assert_eq!(bar_segment(s), Some(BarSegment::Failed));
        }
        assert_eq!(bar_segment(Building), Some(BarSegment::Building));
        for s in [Queued, Created, FailedTransient] {
            assert_eq!(bar_segment(s), Some(BarSegment::Queued));
        }
        assert_eq!(bar_segment(Substituted), Some(BarSegment::Substituted));
        assert_eq!(bar_segment(Aborted), Some(BarSegment::Aborted));
        assert_eq!(
            bar_segment(DependencyFailed),
            Some(BarSegment::DependencyFailed)
        );
        assert_eq!(
            bar_segment(Skipped),
            None,
            "settled work with no result is not a segment, or the bar never fills"
        );
    }

    #[test]
    fn total_counts_every_segment_including_substituted() {
        let mut c = BuildStatusCounts::default();
        c.add(BuildStatus::Completed, 3);
        c.add(BuildStatus::FailedPermanent, 2);
        c.add(BuildStatus::Building, 1);
        c.add(BuildStatus::Queued, 4);
        c.add(BuildStatus::Substituted, 9000);
        c.add(BuildStatus::Aborted, 5);
        c.add(BuildStatus::DependencyFailed, 7);
        assert_eq!(c.completed, 3);
        assert_eq!(c.failed, 2);
        assert_eq!(c.building, 1);
        assert_eq!(c.queued, 4);
        assert_eq!(c.substituted, 9000);
        assert_eq!(c.aborted, 5);
        assert_eq!(c.dependency_failed, 7);
        assert_eq!(c.total(), 9022);
    }
}
