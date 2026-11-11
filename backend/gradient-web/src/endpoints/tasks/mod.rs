/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod evaluations;
pub mod flake_inputs;
pub mod management;
pub mod metrics;
pub mod provision;
pub mod pull_requests;
mod refusal;
pub mod repository;
pub mod triggers;

pub use self::evaluations::{
    EntryPointDownloadQuery, EntryPointsQuery, EvaluateRequest, EvaluationsQuery,
    get_entry_point_download, get_task_details, get_task_entry_points, get_task_evaluations,
    post_task_evaluate,
};
pub use self::management::{
    CheckRepositoryRequest, MakeTaskRequest, PatchTaskRequest, TransferOwnershipRequest,
    delete_task, delete_task_active, get, get_task, get_task_name_available, patch_task,
    post_task_active, post_task_check_repository, post_task_transfer, put,
};
pub use self::metrics::{EntryPointMetricsQuery, get_entry_point_metrics, get_task_metrics};

use crate::endpoints::evals::task_run_history::HistoryBar;
use crate::endpoints::evals::types::EvaluationAbortReplacementRetryPhaseStartsAndExpectedDuration;
use crate::permissions::{Permission, PermissionMask, mask_grants, mask_to_vec};
use gradient_types::ids::*;

use gradient_db::evaluations::LatestEvaluation;
use gradient_db::evaluations::failed_attributes::FailedAttribute;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::triggers::{ConcurrencyPolicy, TriggerConfig, next_poll_time_with_jitter};
use gradient_types::{
    EvaluationProgress, MTask, MTaskTrigger, TaskTriggerId, TriggerType, WaitingReason,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
    pub repository_fetch: Option<RepositoryFetchStatus>,
    pub branches: Vec<Option<String>>,
    pub next_polling_trigger_check_at: Option<chrono::NaiveDateTime>,
    pub can_edit: bool,
    pub can_trigger: bool,
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct ActiveTriggerBranchesAndNextPollTime {
    pub branches: Vec<Option<String>>,
    pub next_polling_trigger_check_at: Option<chrono::NaiveDateTime>,
}

impl ActiveTriggerBranchesAndNextPollTime {
    pub(crate) fn of<'a>(
        task: &MTask,
        triggers: impl IntoIterator<Item = &'a MTaskTrigger>,
    ) -> Self {
        let mut branches = BTreeSet::new();
        let mut next_poll: Option<chrono::NaiveDateTime> = None;
        for trigger in triggers
            .into_iter()
            .filter(|t| t.active && t.task == task.id)
        {
            let Ok(config) = TriggerConfig::parse_row(trigger.trigger_type, &trigger.config) else {
                continue;
            };
            branches.extend(config.branch_names_or_patterns_for_task_header());
            if let TriggerConfig::Polling { interval_secs, .. } = config
                && task.active
            {
                let due = trigger.last_fired_at.map_or(trigger.created_at, |t| {
                    next_poll_time_with_jitter(trigger.id, t, interval_secs)
                });
                next_poll = Some(next_poll.map_or(due, |n| n.min(due)));
            }
        }
        Self {
            branches: branches.into_iter().collect(),
            next_polling_trigger_check_at: next_poll,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct RepositoryFetchStatus {
    pub checked_at: chrono::NaiveDateTime,
    pub ok_at: Option<chrono::NaiveDateTime>,
    pub failed: bool,
    pub error: Option<String>,
}

impl RepositoryFetchStatus {
    pub fn of(task: &MTask, can_edit: bool) -> Option<Self> {
        Some(Self {
            checked_at: task.last_fetch_at?,
            ok_at: task.last_fetch_ok_at,
            failed: task.last_fetch_error.is_some(),
            error: error_message_if_caller_can_edit_task(task.last_fetch_error.clone(), can_edit),
        })
    }
}

/// Only members who can edit the task can view those errors. Git errors can contain internal hostnames.
pub(crate) fn error_message_if_caller_can_edit_task(
    error: Option<String>,
    can_edit: bool,
) -> Option<String> {
    error.filter(|_| can_edit)
}

impl TaskResponse {
    pub fn new(
        task: MTask,
        latest_evaluation: Option<LatestEvaluation>,
        recent_evaluations: Vec<HistoryBar>,
        triggers: &[MTaskTrigger],
        permissions: PermissionMask,
    ) -> Self {
        let repository_fetch =
            RepositoryFetchStatus::of(&task, mask_grants(permissions, Permission::EditTask));
        let ActiveTriggerBranchesAndNextPollTime {
            branches,
            next_polling_trigger_check_at,
        } = ActiveTriggerBranchesAndNextPollTime::of(&task, triggers);
        Self {
            id: task.id,
            project: task.project,
            name: task.name,
            active: task.active,
            display_name: task.display_name,
            description: task.description,
            repository: task.repository,
            wildcard: task.wildcard,
            last_evaluation: latest_evaluation.map(|e| e.id),
            last_evaluation_status: latest_evaluation.map(|e| e.status),
            recent_evaluations,
            force_evaluation: task.force_evaluation,
            created_by: task.created_by,
            created_at: task.created_at,
            managed: task.managed,
            keep_evaluations: task.keep_evaluations,
            concurrency: task.concurrency,
            wait_for_workers: task.wait_for_workers,
            retry_failed_builds: task.retry_failed_builds,
            multi_node_evaluation: task.multi_node_evaluation,
            exec_enabled: task.exec_enabled,
            only_evaluate: task.only_evaluate,
            repository_fetch,
            branches,
            next_polling_trigger_check_at,
            can_edit: mask_grants(permissions, Permission::EditTask),
            can_trigger: mask_grants(permissions, Permission::TriggerEvaluation),
            permissions: mask_to_vec(permissions),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointSummary {
    pub id: EntryPointId,
    pub build_id: BuildJobId,
    pub derivation_path: String,
    pub eval: String,
    pub build_status: gradient_entity::build::BuildStatus,
    pub has_artefacts: bool,
    pub outputs: std::collections::BTreeMap<String, String>,
    #[schema(value_type = String)]
    pub architecture: gradient_entity::server::Architecture,
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

impl From<FailedAttribute> for FailedAttributeSummary {
    fn from(failed: FailedAttribute) -> Self {
        Self {
            eval: failed.attr,
            message: failed.message,
        }
    }
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

impl EvaluationTriggerSummary {
    pub fn new(id: TaskTriggerId, trigger_type: TriggerType) -> Self {
        Self { id, trigger_type }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct DeletedResponse {
    pub deleted: bool,
}

impl DeletedResponse {
    pub fn deleted() -> Self {
        Self { deleted: true }
    }
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

impl TaskDetailsResponse {
    pub fn new(
        task: MTask,
        queue: QueueSummary,
        last_evaluations: Vec<EvaluationSummary>,
        triggers: &[MTaskTrigger],
        permissions: PermissionMask,
    ) -> Self {
        let repository_fetch =
            RepositoryFetchStatus::of(&task, mask_grants(permissions, Permission::EditTask));
        let ActiveTriggerBranchesAndNextPollTime {
            branches,
            next_polling_trigger_check_at,
        } = ActiveTriggerBranchesAndNextPollTime::of(&task, triggers);
        Self {
            id: task.id,
            name: task.name,
            display_name: task.display_name,
            description: task.description,
            repository: task.repository,
            wildcard: task.wildcard,
            active: task.active,
            created_at: task.created_at,
            keep_evaluations: task.keep_evaluations,
            last_check_at: checked_at(task.last_check_at),
            repository_fetch,
            branches,
            next_polling_trigger_check_at,
            queue,
            last_evaluations,
            can_edit: mask_grants(permissions, Permission::EditTask),
            can_trigger: mask_grants(permissions, Permission::TriggerEvaluation),
            permissions: mask_to_vec(permissions),
            managed: task.managed,
        }
    }
}

fn checked_at(t: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    (t != *gradient_types::NULL_TIME).then_some(t)
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

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, utoipa::ToSchema)]
pub struct QueueSummary {
    pub building: i64,
    pub queued: i64,
}

#[cfg(test)]
mod rollup_tests {
    use super::{BarSegment, BuildStatusCounts, bar_segment};
    use gradient_entity::build::BuildStatus;

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

#[cfg(test)]
mod summary_tests {
    use super::*;

    fn summary(progress: Option<EvaluationProgress>) -> serde_json::Value {
        serde_json::to_value(EvaluationSummary {
            id: EvaluationId::nil(),
            commit: String::new(),
            commit_message: None,
            status: EvaluationStatus::Fetching,
            wildcard: String::new(),
            trigger: None,
            triggered_by: None,
            pr_number: None,
            total_builds: 0,
            builds: BuildStatusCounts::default(),
            errors: 0,
            warnings: 0,
            dispatched_job: None,
            cluster_job: None,
            prioritized: false,
            only_evaluate: false,
            created_at: chrono::NaiveDateTime::default(),
            started_at: None,
            finished_at: None,
            updated_at: chrono::NaiveDateTime::default(),
            expected_thunks: progress.as_ref().map(|_| 450),
            progress,
            waiting_reason: None,
            history: Default::default(),
        })
        .unwrap()
    }

    #[test]
    fn the_summary_carries_progress_only_while_there_is_some() {
        let body = summary(Some(EvaluationProgress::Evaluating { thunks: 7 }));
        assert_eq!(
            body["progress"],
            serde_json::json!({ "kind": "evaluating", "thunks": 7 })
        );
        assert_eq!(body["expected_thunks"], 450);
        let idle = summary(None);
        assert!(idle.get("progress").is_none());
        assert!(idle.get("expected_thunks").is_none());
    }

    #[test]
    fn the_summary_always_says_whether_the_evaluation_only_evaluates() {
        assert_eq!(summary(None)["only_evaluate"], false);
    }

    #[test]
    fn checked_at_maps_null_time_sentinel_to_none() {
        assert_eq!(checked_at(*gradient_types::NULL_TIME), None);
        let t = chrono::NaiveDate::from_ymd_opt(2026, 6, 13)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert_eq!(checked_at(t), Some(t));
    }
}

#[cfg(test)]
mod repository_fetch_tests {
    use super::*;

    fn task(error: Option<&str>) -> MTask {
        MTask {
            last_fetch_at: Some(chrono::NaiveDateTime::default()),
            last_fetch_error: error.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn never_fetched_has_no_status() {
        assert_eq!(RepositoryFetchStatus::of(&MTask::default(), true), None);
    }

    #[test]
    fn failure_text_only_reaches_editors() {
        let failed = task(Some("Permission denied (publickey)"));
        let editor = RepositoryFetchStatus::of(&failed, true).unwrap_or_else(|| panic!("status"));
        assert!(editor.failed);
        assert_eq!(
            editor.error.as_deref(),
            Some("Permission denied (publickey)")
        );

        let viewer = RepositoryFetchStatus::of(&failed, false).unwrap_or_else(|| panic!("status"));
        assert!(viewer.failed);
        assert_eq!(viewer.error, None);
    }
}

#[cfg(test)]
mod trigger_summary_tests {
    use super::*;

    fn at(s: &str) -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn trigger(task: &MTask, config: TriggerConfig, active: bool) -> MTaskTrigger {
        MTaskTrigger {
            id: TaskTriggerId::now_v7(),
            task: task.id,
            trigger_type: config.trigger_type(),
            config: config.to_db_json(),
            active,
            created_at: at("2026-11-01 10:00:00"),
            ..Default::default()
        }
    }

    #[test]
    fn branches_come_from_active_triggers_only_and_never_fired_polls_are_due_at_creation() {
        let task = MTask {
            id: TaskId::now_v7(),
            active: true,
            ..Default::default()
        };
        let triggers = [
            trigger(
                &task,
                TriggerConfig::Polling {
                    interval_secs: 300,
                    branch: None,
                },
                true,
            ),
            trigger(
                &task,
                TriggerConfig::Time {
                    cron: "0 0 3 * * *".into(),
                    branch: Some("release".into()),
                },
                true,
            ),
            trigger(
                &task,
                TriggerConfig::Polling {
                    interval_secs: 300,
                    branch: Some("paused".into()),
                },
                false,
            ),
        ];
        let summary = ActiveTriggerBranchesAndNextPollTime::of(&task, &triggers);
        assert_eq!(summary.branches, vec![None, Some("release".to_owned())]);
        assert_eq!(
            summary.next_polling_trigger_check_at,
            Some(at("2026-11-01 10:00:00"))
        );
    }

    #[test]
    fn a_fired_poll_is_due_one_interval_later_and_a_paused_task_never_polls() {
        let mut task = MTask {
            id: TaskId::now_v7(),
            active: true,
            ..Default::default()
        };
        let mut polling = trigger(
            &task,
            TriggerConfig::Polling {
                interval_secs: 100,
                branch: None,
            },
            true,
        );
        polling.last_fired_at = Some(at("2026-11-01 12:00:00"));
        let triggers = [polling];
        let due = ActiveTriggerBranchesAndNextPollTime::of(&task, &triggers)
            .next_polling_trigger_check_at
            .unwrap();
        assert!((at("2026-11-01 12:01:40")..=at("2026-11-01 12:01:50")).contains(&due));

        task.active = false;
        assert_eq!(
            ActiveTriggerBranchesAndNextPollTime::of(&task, &triggers)
                .next_polling_trigger_check_at,
            None
        );
    }
}
