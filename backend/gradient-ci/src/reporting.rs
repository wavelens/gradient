/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::CiStatus;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_types::events::names;

pub fn eval_kind_str(kind: EvaluationKind) -> &'static str {
    match kind {
        EvaluationKind::Normal => "normal",
        EvaluationKind::InputUpdate => "input_update",
        EvaluationKind::DrvRecovery => "drv_recovery",
        EvaluationKind::Ssh => "ssh",
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
}

pub fn check_context_kind_for_event(event: &str) -> Option<CheckContextKind> {
    match event {
        names::EVALUATION_ACTION_REQUIRED => Some(CheckContextKind::Approval),
        names::EVALUATION_APPROVAL_GRANTED => Some(CheckContextKind::Approval),
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
        _ => None,
    }
}

/// The Evaluation check is concluding once the evaluation is `Building`, red when an attribute
/// failed to evaluate. A later `Failure` or `Error` is a per-Build concern.
pub fn suppress_evaluation_failure(status: &CiStatus, reached_building: bool) -> bool {
    reached_building && matches!(status, CiStatus::Failure | CiStatus::Error)
}

pub fn evaluation_check_status(status: CiStatus, any_attribute_failed: bool) -> CiStatus {
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

pub fn ci_status_for_evaluation(status: &EvaluationStatus) -> Option<CiStatus> {
    match status {
        EvaluationStatus::Completed => Some(CiStatus::Success),
        EvaluationStatus::Failed => Some(CiStatus::Failure),
        EvaluationStatus::Aborted => Some(CiStatus::Error),
        EvaluationStatus::Queued
        | EvaluationStatus::Fetching
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation
        | EvaluationStatus::Building
        | EvaluationStatus::Waiting => None,
    }
}

pub fn ci_status_for_build(status: &BuildStatus) -> Option<CiStatus> {
    match status {
        BuildStatus::Building => Some(CiStatus::Running),
        BuildStatus::Completed | BuildStatus::Substituted => Some(CiStatus::Success),
        BuildStatus::FailedPermanent
        | BuildStatus::FailedTimeout
        | BuildStatus::DependencyFailed => Some(CiStatus::Failure),
        BuildStatus::Aborted => Some(CiStatus::Error),
        BuildStatus::Created
        | BuildStatus::Queued
        | BuildStatus::FailedTransient
        | BuildStatus::Skipped => None,
    }
}

/// `Created` is posting `build.created` as a pending check right after the entry point evaluation.
/// Already-cached derivations never transition and would otherwise show no check.
#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::events::build::Reported;

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
    fn suppresses_eval_failure_only_after_building() {
        assert!(suppress_evaluation_failure(&CiStatus::Failure, true));
        assert!(suppress_evaluation_failure(&CiStatus::Error, true));
        assert!(!suppress_evaluation_failure(&CiStatus::Failure, false));
        assert!(!suppress_evaluation_failure(&CiStatus::Success, true));
        assert!(!suppress_evaluation_failure(&CiStatus::Pending, true));
    }

    #[test]
    fn a_failed_attribute_turns_the_evaluation_check_red() {
        assert_eq!(
            evaluation_check_status(CiStatus::Success, true),
            CiStatus::Failure
        );
        assert_eq!(
            evaluation_check_status(CiStatus::Success, false),
            CiStatus::Success
        );
        assert_eq!(
            evaluation_check_status(CiStatus::Running, true),
            CiStatus::Running
        );
    }

    #[test]
    fn evaluation_status_maps_terminal_states_and_skips_the_rest() {
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Completed),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Failed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Aborted),
            Some(CiStatus::Error)
        );
        for s in [
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
            EvaluationStatus::Waiting,
        ] {
            assert_eq!(ci_status_for_evaluation(&s), None);
        }
    }

    #[test]
    fn build_status_maps_every_stage_to_a_check_state() {
        assert_eq!(
            ci_status_for_build(&BuildStatus::Completed),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Substituted),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Building),
            Some(CiStatus::Running)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::FailedPermanent),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::DependencyFailed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Aborted),
            Some(CiStatus::Error)
        );
        for s in [BuildStatus::Created, BuildStatus::Queued] {
            assert_eq!(ci_status_for_build(&s), None);
        }
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
        assert_eq!(
            crate::actions::git_host_status_for_event("build.created"),
            Some(CiStatus::Pending)
        );
    }

    #[test]
    fn build_event_dependency_failure_and_abort_are_failures() {
        for s in [
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTimeout,
            BuildStatus::DependencyFailed,
            BuildStatus::Aborted,
        ] {
            assert_eq!(Reported::reports(s), Some("build.failed"));
            assert_eq!(
                crate::actions::git_host_status_for_event(Reported::reports(s).unwrap()),
                Some(CiStatus::Failure)
            );
        }
    }
}
