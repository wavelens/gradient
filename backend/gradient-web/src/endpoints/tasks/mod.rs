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
pub mod repository_status_check;
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
use crate::permissions::{Permission, PermissionMask, mask_grants, mask_to_vec};

pub use gradient_api_types::{
    BuildStatusCounts, DeletedResponse, EntryPointSummary, EvaluationSummary,
    EvaluationTriggerSummary, FailedAttributeSummary, PaginatedEntryPoints, QueueSummary,
    RepositoryFetchStatus, TaskDetailsResponse, TaskResponse,
};
use gradient_db::evaluations::LatestEvaluation;
use gradient_types::triggers::{TriggerConfig, next_poll_time_with_jitter};
use gradient_types::{MTask, MTaskTrigger};
use std::collections::BTreeSet;

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

pub fn repository_fetch_status_for_task_row_when_caller_can_edit_task(
    task: &MTask,
    can_edit: bool,
) -> Option<RepositoryFetchStatus> {
    Some(RepositoryFetchStatus {
        checked_at: task.last_fetch_at?,
        ok_at: task.last_fetch_ok_at,
        failed: task.last_fetch_error.is_some(),
        error: error_message_if_caller_can_edit_task(task.last_fetch_error.clone(), can_edit),
    })
}

/// Only members who can edit the task can view those errors. Git errors can contain internal hostnames.
pub(crate) fn error_message_if_caller_can_edit_task(
    error: Option<String>,
    can_edit: bool,
) -> Option<String> {
    error.filter(|_| can_edit)
}

pub fn build_task_response_from_task_row_latest_evaluation_recent_evaluation_bars_triggers_and_caller_permission_mask(
    task: MTask,
    latest_evaluation: Option<LatestEvaluation>,
    recent_evaluations: Vec<HistoryBar>,
    triggers: &[MTaskTrigger],
    permissions: PermissionMask,
) -> TaskResponse {
    let repository_fetch = repository_fetch_status_for_task_row_when_caller_can_edit_task(
        &task,
        mask_grants(permissions, Permission::EditTask),
    );
    let ActiveTriggerBranchesAndNextPollTime {
        branches,
        next_polling_trigger_check_at,
    } = ActiveTriggerBranchesAndNextPollTime::of(&task, triggers);
    TaskResponse {
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
        restrict_evaluation_fetches: task.restrict_evaluation_fetches,
        extra_allowed_uri_prefixes: task.extra_allowed_uri_prefixes,
        repository_fetch,
        branches,
        next_polling_trigger_check_at,
        can_edit: mask_grants(permissions, Permission::EditTask),
        can_trigger: mask_grants(permissions, Permission::TriggerEvaluation),
        permissions: mask_to_vec(permissions),
    }
}

pub fn build_task_details_response_from_task_row_queue_summary_last_evaluations_triggers_and_caller_permission_mask(
    task: MTask,
    queue: QueueSummary,
    last_evaluations: Vec<EvaluationSummary>,
    triggers: &[MTaskTrigger],
    permissions: PermissionMask,
) -> TaskDetailsResponse {
    let repository_fetch = repository_fetch_status_for_task_row_when_caller_can_edit_task(
        &task,
        mask_grants(permissions, Permission::EditTask),
    );
    let ActiveTriggerBranchesAndNextPollTime {
        branches,
        next_polling_trigger_check_at,
    } = ActiveTriggerBranchesAndNextPollTime::of(&task, triggers);
    TaskDetailsResponse {
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

fn checked_at(t: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    (t != *gradient_types::NULL_TIME).then_some(t)
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_types::EvaluationProgress;
    use gradient_types::ids::EvaluationId;

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
        assert_eq!(
            repository_fetch_status_for_task_row_when_caller_can_edit_task(&MTask::default(), true),
            None
        );
    }

    #[test]
    fn failure_text_only_reaches_editors() {
        let failed = task(Some("Permission denied (publickey)"));
        let editor = repository_fetch_status_for_task_row_when_caller_can_edit_task(&failed, true)
            .unwrap_or_else(|| panic!("status"));
        assert!(editor.failed);
        assert_eq!(
            editor.error.as_deref(),
            Some("Permission denied (publickey)")
        );

        let viewer = repository_fetch_status_for_task_row_when_caller_can_edit_task(&failed, false)
            .unwrap_or_else(|| panic!("status"));
        assert!(viewer.failed);
        assert_eq!(viewer.error, None);
    }
}

#[cfg(test)]
mod trigger_summary_tests {
    use super::*;
    use gradient_types::ids::{TaskId, TaskTriggerId};
    use gradient_types::triggers::ChangedFilePathFilter;

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
                    path_filter: ChangedFilePathFilter::default(),
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
                    path_filter: ChangedFilePathFilter::default(),
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
                path_filter: ChangedFilePathFilter::default(),
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
