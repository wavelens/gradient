/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::CiStatus;
use anyhow::{Context, Result};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::events::evaluation::Phase;
use gradient_types::events::{fork_pull_request_approval_events, names};
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::{BuildJobId, EvaluationId, ExecJobId, PullRequestApprovalId};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;

pub fn eval_kind_str(kind: EvaluationKind) -> &'static str {
    match kind {
        EvaluationKind::Normal => "normal",
        EvaluationKind::InputUpdate => "input_update",
        EvaluationKind::DrvRecovery => "drv_recovery",
        EvaluationKind::Ssh => "ssh",
        EvaluationKind::Retry => "retry",
    }
}

pub fn format_check_scope(project_name: Option<&str>, task_name: &str) -> String {
    match project_name {
        Some(project) => format!("{}/{}", project, task_name),
        None => task_name.to_string(),
    }
}

pub fn approval_check_context(task_name: &str) -> String {
    format!("gradient/{}: Approval", task_name)
}

pub fn evaluation_check_context(task_name: &str, wildcard_suffix: Option<&str>) -> String {
    match wildcard_suffix {
        Some(w) => format!("gradient/{}: Evaluation: {}", task_name, w),
        None => format!("gradient/{}: Evaluation", task_name),
    }
}

pub fn build_check_context(task_name: &str, entry_point: &str) -> String {
    format!("gradient/{}: Build {}", task_name, entry_point)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckContextKind {
    Approval,
    Evaluation,
    Build,
    ExecJob,
}

pub fn check_context_kind_for_event(event: &str) -> Option<CheckContextKind> {
    match event {
        names::FORK_PULL_REQUEST_APPROVAL_NEEDS_MAINTAINER_APPROVAL
        | names::FORK_PULL_REQUEST_APPROVAL_EVALUATION_STARTED_AFTER_APPROVAL
        | names::FORK_PULL_REQUEST_APPROVAL_APPROVAL_WITHOUT_CHANGED_FILE_MATCHING_TRIGGER_PATH => {
            Some(CheckContextKind::Approval)
        }
        names::EVALUATION_QUEUED
        | names::EVALUATION_STARTED
        | names::EVALUATION_BUILDING
        | names::EVALUATION_COMPLETED
        | names::EVALUATION_FAILED
        | names::EVALUATION_ABORTED => Some(CheckContextKind::Evaluation),
        names::BUILD_CREATED
        | names::BUILD_QUEUED
        | names::BUILD_STARTED
        | names::BUILD_COMPLETED
        | names::BUILD_FAILED
        | names::BUILD_SUBSTITUTED => Some(CheckContextKind::Build),
        names::EXEC_JOB_STARTED
        | names::EXEC_JOB_COMPLETED
        | names::EXEC_JOB_FAILED
        | names::EXEC_JOB_ABORTED => Some(CheckContextKind::ExecJob),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitStatusCheckWithIds {
    Approval {
        approval: PullRequestApprovalId,
        phase: fork_pull_request_approval_events::Phase,
    },
    Evaluation {
        evaluation: EvaluationId,
    },
    Build {
        evaluation: EvaluationId,
        build_job: BuildJobId,
    },
    ExecJob {
        evaluation: EvaluationId,
        job: ExecJobId,
        entry_point_name: String,
    },
}

#[derive(Deserialize)]
struct EvaluationEventContentId {
    evaluation_id: EvaluationId,
}

#[derive(Deserialize)]
struct ApprovalEventContentId {
    approval_id: PullRequestApprovalId,
    #[serde(default)]
    phase: fork_pull_request_approval_events::Phase,
}

#[derive(Deserialize)]
struct BuildEventIds {
    build_id: BuildJobId,
    evaluation_id: EvaluationId,
}

#[derive(Deserialize)]
struct ExecJobEventContentIdFields {
    job: ExecJobId,
    evaluation: EvaluationId,
    entry_point_name: Option<String>,
}

fn deserialize_id_from_event_content<T: DeserializeOwned>(
    event: &str,
    content: &JsonValue,
) -> Result<T> {
    T::deserialize(content)
        .with_context(|| format!("parse the IDs from the content of the {event} event"))
}

impl CommitStatusCheckWithIds {
    pub fn commit_status_check_from_event_name_and_content(
        event: &str,
        content: &JsonValue,
    ) -> Result<Option<Self>> {
        let Some(kind) = check_context_kind_for_event(event) else {
            return Ok(None);
        };
        Ok(Some(match kind {
            CheckContextKind::Approval => {
                let ids: ApprovalEventContentId =
                    deserialize_id_from_event_content(event, content)?;
                Self::Approval {
                    approval: ids.approval_id,
                    phase: ids.phase,
                }
            }
            CheckContextKind::Evaluation => {
                let ids: EvaluationEventContentId =
                    deserialize_id_from_event_content(event, content)?;
                Self::Evaluation {
                    evaluation: ids.evaluation_id,
                }
            }
            CheckContextKind::Build => {
                let ids: BuildEventIds = deserialize_id_from_event_content(event, content)?;
                Self::Build {
                    evaluation: ids.evaluation_id,
                    build_job: ids.build_id,
                }
            }
            CheckContextKind::ExecJob => {
                let ids: ExecJobEventContentIdFields =
                    deserialize_id_from_event_content(event, content)?;
                let Some(entry_point_name) = ids.entry_point_name else {
                    return Ok(None);
                };
                Self::ExecJob {
                    evaluation: ids.evaluation,
                    job: ids.job,
                    entry_point_name,
                }
            }
        }))
    }

    pub fn evaluation_id(&self) -> Option<EvaluationId> {
        match self {
            Self::Approval { .. } => None,
            Self::Evaluation { evaluation }
            | Self::Build { evaluation, .. }
            | Self::ExecJob { evaluation, .. } => Some(*evaluation),
        }
    }
}

/// The Evaluation check can show only evaluation progress after Evaluation reached Building. The
/// result is green, or red if an attribute failed to evaluate. Later Failure or Error values come
/// from builds and belong to per-build checks.
pub fn evaluation_check_status(
    status: CiStatus,
    reached_building: bool,
    any_attribute_failed: bool,
) -> CiStatus {
    let status = match status {
        CiStatus::Failure | CiStatus::Error if reached_building => CiStatus::Success,
        other => other,
    };
    match status {
        CiStatus::Success if any_attribute_failed => CiStatus::Failure,
        other => other,
    }
}

pub fn failed_attributes_description(count: usize) -> String {
    match count {
        1 => "1 attribute failed to evaluate".to_owned(),
        n => format!("{n} attributes failed to evaluate"),
    }
}

pub fn ci_status_for_evaluation(status: &EvaluationStatus) -> CiStatus {
    match status {
        EvaluationStatus::Queued | EvaluationStatus::Waiting => CiStatus::Pending,
        EvaluationStatus::Fetching
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation => CiStatus::Running,
        EvaluationStatus::Building | EvaluationStatus::Completed => CiStatus::Success,
        EvaluationStatus::Failed => CiStatus::Failure,
        EvaluationStatus::Aborted => CiStatus::Error,
    }
}

pub fn queued_or_waiting_evaluation_description(
    status: EvaluationStatus,
    reason: Option<WaitingReason>,
) -> Option<&'static str> {
    Phase::of_created(status, reason).and_then(|(_, description)| description)
}

pub fn fork_pull_request_pending_approval_status_description(commit_hex: &str) -> String {
    format!(
        "Waiting for maintainer approval of this fork pull request commit. Comment /gradient approve {commit_hex}"
    )
}

pub fn fork_pull_request_approval_check_description(
    approved: bool,
    phase: fork_pull_request_approval_events::Phase,
    commit_hex: &str,
) -> Option<String> {
    match phase {
        _ if !approved => Some(fork_pull_request_pending_approval_status_description(
            commit_hex,
        )),
        fork_pull_request_approval_events::Phase::ApprovalWithoutChangedFileMatchingTriggerPath => {
            Some(
                "Approved. No evaluation started because the changed files don't match the path filter of the trigger."
                    .to_owned(),
            )
        }
        _ => None,
    }
}

pub fn ci_status_for_approval_check(approved: bool) -> CiStatus {
    if approved {
        CiStatus::Success
    } else {
        CiStatus::ActionRequired
    }
}

pub fn ci_status_for_exec_job(status: ExecJobStatus) -> Option<CiStatus> {
    match status {
        ExecJobStatus::Running => Some(CiStatus::Running),
        ExecJobStatus::Completed => Some(CiStatus::Success),
        ExecJobStatus::Failed => Some(CiStatus::Failure),
        ExecJobStatus::Aborted => Some(CiStatus::Error),
        ExecJobStatus::Created | ExecJobStatus::Queued | ExecJobStatus::Starting => None,
    }
}

pub fn ci_status_for_build(status: &BuildStatus) -> Option<CiStatus> {
    match status {
        BuildStatus::Created | BuildStatus::Queued => Some(CiStatus::Pending),
        BuildStatus::Building => Some(CiStatus::Running),
        BuildStatus::Completed | BuildStatus::Substituted => Some(CiStatus::Success),
        BuildStatus::FailedPermanent
        | BuildStatus::FailedTimeout
        | BuildStatus::DependencyFailed => Some(CiStatus::Failure),
        BuildStatus::Aborted => Some(CiStatus::Error),
        BuildStatus::FailedTransient | BuildStatus::Skipped => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::events::build::Reported;
    use gradient_types::events::{evaluation, exec_job, fork_pull_request_approval_events};

    #[test]
    fn check_scope_uses_the_project_when_present() {
        assert_eq!(
            format_check_scope(Some("wavelens"), "my-task"),
            "wavelens/my-task"
        );
        assert_eq!(format_check_scope(None, "my-task"), "my-task");
    }

    #[test]
    fn evaluation_context_includes_the_wildcard_when_bounded() {
        assert_eq!(
            evaluation_check_context("my-task", None),
            "gradient/my-task: Evaluation"
        );
        assert_eq!(
            evaluation_check_context("my-task", Some("packages.x86_64-linux.foo")),
            "gradient/my-task: Evaluation: packages.x86_64-linux.foo"
        );
    }

    #[test]
    fn a_failed_attribute_turns_the_evaluation_check_red() {
        for (status, reached_building, any_attribute_failed, expected) in [
            (CiStatus::Success, true, true, CiStatus::Failure),
            (CiStatus::Success, true, false, CiStatus::Success),
            (CiStatus::Running, false, true, CiStatus::Running),
            (CiStatus::Failure, false, false, CiStatus::Failure),
            (CiStatus::Error, false, true, CiStatus::Error),
            (CiStatus::Failure, true, false, CiStatus::Success),
            (CiStatus::Failure, true, true, CiStatus::Failure),
            (CiStatus::Error, true, false, CiStatus::Success),
        ] {
            assert_eq!(
                evaluation_check_status(status.clone(), reached_building, any_attribute_failed),
                expected,
                "{status:?} reached_building={reached_building} any_attribute_failed={any_attribute_failed}"
            );
        }
    }

    #[test]
    fn evaluation_status_to_commit_status_maps_every_status_and_waiting_description() {
        for (status, expected) in [
            (EvaluationStatus::Queued, CiStatus::Pending),
            (EvaluationStatus::Waiting, CiStatus::Pending),
            (EvaluationStatus::Fetching, CiStatus::Running),
            (EvaluationStatus::EvaluatingFlake, CiStatus::Running),
            (EvaluationStatus::EvaluatingDerivation, CiStatus::Running),
            (EvaluationStatus::Building, CiStatus::Success),
            (EvaluationStatus::Completed, CiStatus::Success),
            (EvaluationStatus::Failed, CiStatus::Failure),
            (EvaluationStatus::Aborted, CiStatus::Error),
        ] {
            assert_eq!(ci_status_for_evaluation(&status), expected, "{status:?}");
        }
        assert_eq!(
            queued_or_waiting_evaluation_description(
                EvaluationStatus::Waiting,
                Some(WaitingReason::NoCache)
            ),
            Some("Waiting for a writable cache subscription before this evaluation can run.")
        );
        assert_eq!(
            queued_or_waiting_evaluation_description(EvaluationStatus::Building, None),
            None
        );
    }

    #[test]
    fn every_build_status_maps_to_commit_status_or_no_report() {
        for (status, expected) in [
            (BuildStatus::Created, Some(CiStatus::Pending)),
            (BuildStatus::Queued, Some(CiStatus::Pending)),
            (BuildStatus::Building, Some(CiStatus::Running)),
            (BuildStatus::Completed, Some(CiStatus::Success)),
            (BuildStatus::Substituted, Some(CiStatus::Success)),
            (BuildStatus::FailedPermanent, Some(CiStatus::Failure)),
            (BuildStatus::FailedTimeout, Some(CiStatus::Failure)),
            (BuildStatus::DependencyFailed, Some(CiStatus::Failure)),
            (BuildStatus::Aborted, Some(CiStatus::Error)),
            (BuildStatus::FailedTransient, None),
            (BuildStatus::Skipped, None),
        ] {
            assert_eq!(ci_status_for_build(&status), expected, "{status:?}");
        }
    }

    #[test]
    fn approval_check_is_action_required_until_a_maintainer_approves() {
        assert_eq!(
            ci_status_for_approval_check(false),
            CiStatus::ActionRequired
        );
        assert_eq!(ci_status_for_approval_check(true), CiStatus::Success);
        let commit_hex = "3f786850e387550fdab836ed7e6dc881de23001b";
        assert_eq!(
            fork_pull_request_approval_check_description(
                false,
                fork_pull_request_approval_events::Phase::NeedsMaintainerApproval,
                commit_hex
            ),
            Some(fork_pull_request_pending_approval_status_description(
                commit_hex
            ))
        );
        assert_eq!(
            fork_pull_request_approval_check_description(
                true,
                fork_pull_request_approval_events::Phase::EvaluationStartedAfterApproval,
                commit_hex
            ),
            None
        );
        let description = fork_pull_request_approval_check_description(
            true,
            fork_pull_request_approval_events::Phase::ApprovalWithoutChangedFileMatchingTriggerPath,
            commit_hex,
        )
        .unwrap_or_default();
        assert!(description.contains("path filter"), "{description}");
        assert!(description.chars().count() <= 140, "{description}");
    }

    #[test]
    fn both_approval_events_name_the_approval_check_of_their_approval_row() {
        let approval = PullRequestApprovalId::now_v7();
        for phase in [
            fork_pull_request_approval_events::Phase::NeedsMaintainerApproval,
            fork_pull_request_approval_events::Phase::EvaluationStartedAfterApproval,
            fork_pull_request_approval_events::Phase::ApprovalWithoutChangedFileMatchingTriggerPath,
        ] {
            let content =
                gradient_types::Event::from(fork_pull_request_approval_events::Reported {
                    approval_id: approval,
                    phase,
                    ..Default::default()
                })
                .content();
            assert_eq!(
                CommitStatusCheckWithIds::commit_status_check_from_event_name_and_content(
                    phase.name(),
                    &content
                )
                .unwrap(),
                Some(CommitStatusCheckWithIds::Approval { approval, phase })
            );
        }
    }

    #[test]
    fn pending_approval_status_description_for_full_40_character_commit_hash_names_gradient_approve_command_and_fits_140_character_github_limit()
     {
        let commit_hex = "3f786850e387550fdab836ed7e6dc881de23001b";
        let description = fork_pull_request_pending_approval_status_description(commit_hex);
        assert!(
            description.contains(&format!("/gradient approve {commit_hex}")),
            "{description}"
        );
        assert!(
            description.chars().count() <= 140,
            "{} characters: {description}",
            description.chars().count()
        );
    }

    #[test]
    fn exec_job_status_maps_running_and_final_states() {
        assert_eq!(
            ci_status_for_exec_job(ExecJobStatus::Completed),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_exec_job(ExecJobStatus::Failed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_exec_job(ExecJobStatus::Aborted),
            Some(CiStatus::Error)
        );
        assert_eq!(
            ci_status_for_exec_job(ExecJobStatus::Running),
            Some(CiStatus::Running)
        );
        assert_eq!(ci_status_for_exec_job(ExecJobStatus::Queued), None);
    }

    #[test]
    fn build_job_events_name_same_commit_status_check_while_exec_event_without_entry_point_and_evaluation_waiting_event_name_no_check()
     {
        let content = |status: BuildStatus| {
            gradient_types::Event::from(Reported {
                build_id: BuildJobId::nil(),
                evaluation_id: EvaluationId::nil(),
                status: i16::from(status),
                ..Default::default()
            })
            .content()
        };
        let check = CommitStatusCheckWithIds::commit_status_check_from_event_name_and_content;
        let expected = Some(CommitStatusCheckWithIds::Build {
            evaluation: EvaluationId::nil(),
            build_job: BuildJobId::nil(),
        });

        assert_eq!(
            check("build.queued", &content(BuildStatus::Queued)).unwrap(),
            expected
        );
        assert_eq!(
            check("build.completed", &content(BuildStatus::Completed)).unwrap(),
            expected
        );
        let exec_job = gradient_types::Event::from(exec_job::Reported::default()).content();
        assert_eq!(check("exec_job.completed", &exec_job).unwrap(), None);
        let waiting = gradient_types::Event::from(evaluation::Reported {
            phase: Phase::Waiting,
            ..Default::default()
        })
        .content();
        assert_eq!(check("evaluation.waiting", &waiting).unwrap(), None);
        assert!(
            check(
                "build.completed",
                &serde_json::json!({ "build_id": "not-a-uuid" })
            )
            .is_err()
        );
    }

    #[test]
    fn build_event_reports_live_progress_and_pending_checks() {
        assert_eq!(Reported::reports(BuildStatus::Queued), Some("build.queued"));
        assert_eq!(
            Reported::reports(BuildStatus::Building),
            Some("build.started")
        );
        assert_eq!(
            Reported::reports(BuildStatus::Completed),
            Some("build.completed")
        );
        assert_eq!(
            Reported::reports(BuildStatus::Substituted),
            Some("build.substituted")
        );
        assert_eq!(
            Reported::reports(BuildStatus::Created),
            Some("build.created")
        );
        assert_eq!(
            check_context_kind_for_event("build.created"),
            Some(CheckContextKind::Build)
        );
    }

    #[test]
    fn every_build_failure_kind_and_abort_fires_build_failed_event_name() {
        for s in [
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTimeout,
            BuildStatus::DependencyFailed,
            BuildStatus::Aborted,
        ] {
            assert_eq!(Reported::reports(s), Some("build.failed"));
        }
    }
}
